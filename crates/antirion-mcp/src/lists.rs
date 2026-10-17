//! Reading upstream lists (every page, cached per server and credential), merging them into the
//! gateway's view (visibility, namespacing, ambiguity), and paging the result with sealed cursors.

use std::collections::HashMap;
use std::sync::Arc;

use antirion_core::Stage;
use antirion_error::Message;
use serde_json::{Value, json};

use crate::compile::ServerRt;
use crate::connect::{self, gateway_client_info};
use crate::fingerprint;
use crate::http::Req;
use crate::link::Link;
use crate::names;
use crate::reasons;
use crate::rpc::Fail;
use crate::select::{ItemKind, Selection, credential};
use crate::state::{Hints, ListEntry, ListKey};
use crate::token::CursorClaims;
use crate::upstream::UpError;

const MAX_PAGES: usize = 1000;

/// A list method.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Tools,
    Prompts,
    Resources,
    Templates,
}

impl Kind {
    pub(crate) fn of(method: &str) -> Option<Kind> {
        Some(match method {
            "tools/list" => Kind::Tools,
            "prompts/list" => Kind::Prompts,
            "resources/list" => Kind::Resources,
            "resources/templates/list" => Kind::Templates,
            _ => return None,
        })
    }

    pub(crate) fn method(self) -> &'static str {
        match self {
            Kind::Tools => "tools/list",
            Kind::Prompts => "prompts/list",
            Kind::Resources => "resources/list",
            Kind::Templates => "resources/templates/list",
        }
    }

    /// The member of the result holding the items.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Kind::Tools => "tools",
            Kind::Prompts => "prompts",
            Kind::Resources => "resources",
            Kind::Templates => "resourceTemplates",
        }
    }

    /// The member of an item holding its name or URI.
    pub(crate) fn item_key(self) -> &'static str {
        match self {
            Kind::Tools | Kind::Prompts => "name",
            Kind::Resources => "uri",
            Kind::Templates => "uriTemplate",
        }
    }

    fn item_kind(self) -> ItemKind {
        match self {
            Kind::Tools => ItemKind::Tool,
            Kind::Prompts => ItemKind::Prompt,
            Kind::Resources | Kind::Templates => ItemKind::Resource,
        }
    }

    /// The `list_changed` notification that invalidates this list.
    pub(crate) fn of_notification(method: &str) -> &'static [Kind] {
        match method {
            "notifications/tools/list_changed" => &[Kind::Tools],
            "notifications/prompts/list_changed" => &[Kind::Prompts],
            "notifications/resources/list_changed" => &[Kind::Resources, Kind::Templates],
            _ => &[],
        }
    }
}

/// How a request reaches its servers.
#[derive(Clone, Debug)]
pub(crate) enum Mode {
    /// A legacy session: one link per initialized server.
    Session { links: Vec<Link> },
    /// Legacy without sessions: each use opens and closes an upstream session.
    Stateless { pv: String },
    /// The modern era, with the client's `_meta`.
    Modern { meta: Value },
}

impl Mode {
    /// The link for `server`, and whether it was opened for this use (and must be closed).
    pub(crate) async fn link(&self, req: &Req, server: &Arc<ServerRt>) -> Result<(Link, bool), UpError> {
        match self {
            Mode::Session { links } => links
                .iter()
                .find(|l| l.server.name == server.name)
                .cloned()
                .map(|l| (l, false))
                .ok_or(UpError::SessionGone),
            Mode::Stateless { pv } => {
                let (link, _) =
                    connect::open(req, server, pv, &json!({}), &gateway_client_info(), false).await?;
                Ok((link, true))
            }
            Mode::Modern { meta } => {
                if req.gw.modern_bridge && !crate::discovery::is_modern(req, server, meta).await {
                    let (link, _) = crate::bridge::open_for_modern(req, server, meta).await?;
                    return Ok((link, true));
                }
                Ok((connect::modern_link(req, server, meta), false))
            }
        }
    }

    pub(crate) fn servers(&self) -> Option<Vec<String>> {
        match self {
            Mode::Session { links } => Some(links.iter().map(|l| l.server.name.clone()).collect()),
            _ => None,
        }
    }
}

fn key(req: &Req, server: &ServerRt, kind: Kind) -> ListKey {
    let headers = crate::select::upstream_headers(req, server);
    (server.generation, credential(&headers), kind.method())
}

/// Reads every page of one list through `link`.
async fn fetch(req: &Req, link: &Link, kind: Kind) -> Result<ListEntry, UpError> {
    let env = &req.env;
    let mut items = Vec::new();
    let mut cursor: Option<Value> = None;
    let mut page = None;
    let mut hints = Hints::default();
    for n in 0..MAX_PAGES {
        let params = cursor.take().map(|c| json!({"cursor": c}));
        let msg = link.ask(env, kind.method(), params).await?;
        let Some(result) = msg.get("result") else {
            return Err(UpError::Rejected);
        };
        let got = result
            .get(kind.key())
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if n == 0 {
            hints = Hints::of(result);
        }
        let next = result.get("nextCursor").filter(|c| !c.is_null()).cloned();
        if n == 0 && next.is_some() {
            page = Some(got.len());
        }
        items.extend(got);
        match next {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    if kind == Kind::Tools {
        fingerprint::observe(&env.state, &link.server, &items);
    }
    Ok(ListEntry {
        items: Arc::new(items),
        page,
        hints,
        at: env.now,
    })
}

/// One server's list: from the cache when fresh (unless `fresh`), else read and cached.
pub(crate) async fn entry(
    req: &Req,
    mode: &Mode,
    server: &Arc<ServerRt>,
    kind: Kind,
    fresh: bool,
) -> Result<ListEntry, UpError> {
    let k = key(req, server, kind);
    let state = &req.env.state;
    if !fresh && let Some(e) = state.list(&k, req.env.now, server.tool_cache_ttl) {
        return Ok(e);
    }
    let (link, opened) = mode.link(req, server).await?;
    let start = std::time::Instant::now();
    let read = fetch(req, &link, kind).await;
    if kind == Kind::Tools {
        state.record_contact(
            &server.name,
            req.env.now,
            read.as_ref().err().map(UpError::describe),
        );
    }
    req.env.log.upstream(crate::log::UpstreamCall {
        server: server.name.clone(),
        method: kind.method().to_owned(),
        item: None,
        start,
        end: std::time::Instant::now(),
        error_code: None,
        failed: read.is_err(),
    });
    if opened {
        link.close(&req.env).await;
    }
    let e = read?;
    if !server.tool_cache_ttl.is_zero() {
        state.lists.lock().insert(k, e.clone());
    }
    Ok(e)
}

/// The lists of several servers, read concurrently; failures are reported per server.
pub(crate) async fn entries(
    req: &Req,
    mode: &Mode,
    servers: &[Arc<ServerRt>],
    kind: Kind,
    fresh: bool,
) -> Vec<(Arc<ServerRt>, Result<ListEntry, UpError>)> {
    let reads = servers.iter().map(|s| async move {
        let r = entry(req, mode, s, kind, fresh).await;
        (Arc::clone(s), r)
    });
    futures_util::future::join_all(reads).await
}

/// The merged, client-facing items of several servers' lists.
#[derive(Debug, Default)]
pub(crate) struct Merged {
    pub(crate) items: Vec<Value>,
    pub(crate) page: Option<usize>,
    pub(crate) hints: Vec<Hints>,
}

pub(crate) fn merge(req: &Req, sel: &Selection, kind: Kind, lists: &[(Arc<ServerRt>, ListEntry)]) -> Merged {
    let item_key = kind.item_key();
    let mut offered: HashMap<String, usize> = HashMap::new();
    if !req.gw.prefixed {
        for (_, e) in lists {
            for item in e.items.iter() {
                if let Some(n) = item.get(item_key).and_then(Value::as_str) {
                    *offered.entry(n.to_owned()).or_default() += 1;
                }
            }
        }
    }
    let mut merged = Merged::default();
    let mut paged = false;
    let mut page_total = 0usize;
    for (server, e) in lists {
        merged.hints.push(e.hints.clone());
        paged |= e.page.is_some();
        page_total += e.page.unwrap_or(e.items.len());
        for item in e.items.iter() {
            let Some(name) = item.get(item_key).and_then(Value::as_str) else {
                continue;
            };
            let client = if req.gw.prefixed {
                match kind {
                    Kind::Tools | Kind::Prompts => names::name_out(&server.prefix, name),
                    Kind::Resources | Kind::Templates => names::uri_out(&server.prefix, name),
                }
            } else {
                if offered.get(name).copied().unwrap_or(0) > 1 {
                    continue;
                }
                name.to_owned()
            };
            if !sel.visible(
                &req.env.state,
                server,
                kind.item_kind(),
                name,
                &client,
                Some(item),
            ) {
                continue;
            }
            let mut out = item.clone();
            if req.gw.prefixed {
                names::list_item(&mut out, item_key, &server.prefix);
            }
            merged.items.push(out);
        }
    }
    if paged {
        merged.page = Some(page_total.max(1));
    }
    merged
}

fn invalid_cursor() -> Fail {
    Fail::protocol(
        -32602,
        200,
        reasons::INVALID_CURSOR,
        Stage::Parse,
        Message::Fixed("Invalid cursor."),
    )
}

/// The cursor a client sent, validated: bound to this gateway, principal and method.
pub(crate) fn offset(req: &Req, method: &str, params: Option<&Value>) -> Result<usize, Fail> {
    let Some(cursor) = params.and_then(|p| p.get("cursor")).filter(|c| !c.is_null()) else {
        return Ok(0);
    };
    let claims = cursor
        .as_str()
        .and_then(|c| req.keys.open_cursor(&req.gw.name, c))
        .filter(|c| c.sub == req.caller.principal && c.m == method)
        .ok_or_else(invalid_cursor)?;
    Ok(claims.o)
}

/// One page of a merged list and the cursor of the next one.
pub(crate) fn page(req: &Req, method: &str, merged: Merged, from: usize) -> (Vec<Value>, Option<String>) {
    let total = merged.items.len();
    let Some(size) = merged.page else {
        return (merged.items.into_iter().skip(from).collect(), None);
    };
    let end = from.saturating_add(size).min(total);
    let items: Vec<Value> = merged
        .items
        .into_iter()
        .skip(from)
        .take(end.saturating_sub(from))
        .collect();
    let next = (end < total)
        .then(|| {
            req.keys.seal_cursor(
                &req.gw.name,
                &CursorClaims {
                    sub: req.caller.principal.clone(),
                    m: method.to_owned(),
                    o: end,
                },
            )
        })
        .flatten();
    (items, next)
}

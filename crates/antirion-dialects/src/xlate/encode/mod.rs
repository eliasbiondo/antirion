//! Upstream codecs, request side: the IR encoded for one deployment. Encoding decides the class of
//! every feature for that deployment, so the same pass serves feasibility (a dry run before
//! dispatch) and each attempt's body.

mod anthropic;
mod chat;
mod cohere;
mod converse;
mod gemini;
mod hosted;
mod media;
mod reason;
mod responses;
mod sampling;
mod turns;

use std::collections::{HashMap, HashSet};

use serde_json::Value;

pub use self::media::{Fetched, MediaMap, Need, fetch_needs};
use super::ir::{Block, Choice, Client, Family, Reasoning, Request};
use super::names::{Ids, Names};
use super::target::{Policy, Target, Upstream};
use super::warn::{Kind, Refusal, Src, Warnings};

/// What a response needs from the request translation: names and ids to restore, and whether JSON
/// output was emulated with a forced tool.
#[derive(Clone, Debug, Default)]
pub struct Restore {
    pub names: Names,
    pub ids: Ids,
    pub json_tool: bool,
    /// The client names of Responses custom tools: their calls go back as `custom_tool_call` items.
    pub custom: Vec<String>,
}

/// A translated request body.
#[derive(Clone, Debug)]
pub struct Encoded {
    pub body: Value,
    pub warnings: Warnings,
    pub restore: Restore,
}

/// The name of the tool that emulates structured output.
pub const JSON_TOOL: &str = "json_response";

/// Encodes `req` for `target`. `media` holds the fetched bytes of remote media; `None` is a dry run
/// for feasibility, which assumes every fetch succeeds.
pub fn encode(
    req: &Request,
    target: &Target,
    policy: &Policy,
    media: Option<&MediaMap>,
) -> Result<Encoded, Refusal> {
    if let Some(src) = &req.unpaired {
        return Err(Refusal::unpaired(src));
    }
    let mut e = Enc::new(req, target, policy, media);
    e.common();
    let mut body = match target.dialect {
        Upstream::Chat => chat::encode(&mut e),
        Upstream::Responses => responses::encode(&mut e),
        Upstream::Anthropic => anthropic::encode(&mut e),
        Upstream::Gemini => gemini::encode(&mut e),
        Upstream::Converse => converse::encode(&mut e),
        Upstream::Cohere => cohere::encode(&mut e),
    };
    e.unknown_fields(&mut body);
    if let Some((src, message)) = e.refusals.iter().min_by(|a, b| a.0.order().cmp(b.0.order())) {
        return Err(Refusal::unsupported(src, message.clone()));
    }
    if policy.reject
        && let Some(w) = e.w.first_strict()
    {
        return Err(Refusal::strict(&w));
    }
    Ok(Encoded {
        body,
        warnings: e.w,
        restore: Restore {
            names: e.names,
            ids: e.ids,
            json_tool: e.json_tool,
            custom: req
                .tools
                .iter()
                .filter(|t| t.custom.is_some())
                .map(|t| t.name.clone())
                .collect(),
        },
    })
}

/// The encoding state for one target.
pub(super) struct Enc<'a> {
    pub(super) req: &'a Request,
    pub(super) t: &'a Target,
    pub(super) policy: &'a Policy,
    pub(super) media: Option<&'a MediaMap>,
    pub(super) w: Warnings,
    pub(super) refusals: Vec<(Src, String)>,
    pub(super) names: Names,
    pub(super) ids: Ids,
    pub(super) json_tool: bool,
    /// Cache markers left out (by concrete path), and one-hour markers sent as five-minute ones.
    pub(super) cache_removed: HashSet<String>,
    pub(super) cache_downgraded: HashSet<String>,
}

impl<'a> Enc<'a> {
    fn new(req: &'a Request, t: &'a Target, policy: &'a Policy, media: Option<&'a MediaMap>) -> Enc<'a> {
        let mut all: Vec<&str> = req.tools.iter().map(|x| x.name.as_str()).collect();
        all.extend(req.calls().map(|c| c.name.as_str()));
        if let Some((choice, _)) = &req.choice {
            match choice {
                Choice::Named(n) => all.push(n),
                Choice::Allowed { names, .. } => all.extend(names.iter().map(String::as_str)),
                _ => {}
            }
        }
        Enc {
            req,
            t,
            policy,
            media,
            w: Warnings::default(),
            refusals: Vec::new(),
            names: Names::build(&t.names, all),
            ids: Ids::default(),
            json_tool: false,
            cache_removed: HashSet::new(),
            cache_downgraded: HashSet::new(),
        }
    }

    pub(super) fn warn(&mut self, kind: Kind, src: &Src) {
        self.w.add(kind, src);
    }

    pub(super) fn refuse(&mut self, src: &Src, message: impl Into<String>) {
        self.refusals.push((src.clone(), message.into()));
    }

    pub(super) fn dialect(&self) -> Upstream {
        self.t.dialect
    }

    pub(super) fn openai_client(&self) -> bool {
        matches!(self.req.client, Client::Chat | Client::Responses)
    }

    /// Checks shared by every target: features no translated cell can carry, hosted tools, fields
    /// ignored everywhere, and inert OpenAI fields outside the OpenAI dialects.
    fn common(&mut self) {
        let req = self.req;
        for u in &req.unsupported {
            if matches!(u.src.path(), "audio" | "modalities") && self.dialect() == Upstream::Chat {
                continue;
            }
            self.refuse(&u.src, u.message.clone());
        }
        for h in &req.hosted {
            if h.unsupported {
                self.refuse(
                    &h.src,
                    format!(
                        "The Anthropic-defined tool type `{}` cannot be used with this deployment.",
                        h.kind
                    ),
                );
            } else {
                self.warn(Kind::Dropped, &h.src);
            }
        }
        for src in &req.ignored {
            self.warn(Kind::Ignored, src);
        }
        for t in &req.tools {
            if let Some(Some(grammar)) = &t.custom {
                self.warn(Kind::Approximated, grammar);
            }
        }
        if let Some(c) = &req.auto_cache
            && self.dialect() != Upstream::Anthropic
        {
            self.warn(Kind::Ignored, &c.src);
        }
        if let Some((n, src)) = &req.n
            && *n > 1
            && self.dialect() == Upstream::Responses
        {
            self.refuse(src, "n > 1 cannot be served by a Responses deployment.");
        }
        if !self.t.assistant_prefill
            && req
                .turns
                .last()
                .is_some_and(|t| t.role == super::ir::Role::Assistant)
        {
            self.refuse(
                &req.messages_src,
                "This deployment does not continue a final assistant message.",
            );
        }
        let maps_inert =
            self.openai_client() && matches!(self.dialect(), Upstream::Chat | Upstream::Responses);
        for i in &req.inert {
            let mapped = maps_inert
                && match self.dialect() {
                    Upstream::Chat => !matches!(i.name, "truncation" | "include" | "max_tool_calls"),
                    _ => !matches!(i.name, "prediction"),
                };
            if !mapped {
                self.warn(Kind::Ignored, &i.src);
            }
        }
    }

    /// Unknown client fields: dropped and reported, or copied to the top level when the policy
    /// forwards them (never over a translated field).
    fn unknown_fields(&mut self, body: &mut Value) {
        let req = self.req;
        for u in &req.unknown {
            self.warn(Kind::Unknown, &u.src);
            if u.top_level
                && self.policy.forward_unknown
                && let Some(o) = body.as_object_mut()
                && !o.contains_key(&u.name)
            {
                o.insert(u.name.clone(), u.value.clone());
            }
        }
    }

    /// Whether a reasoning block from history may be sent to this target (its replay scope).
    pub(super) fn replayable(&self, r: &Reasoning) -> bool {
        let t = self.t;
        let payload = r.signature.is_some() || r.redacted.is_some();
        let carrier = match t.dialect {
            Upstream::Chat => false,
            Upstream::Converse => t.family == Family::Anthropic,
            _ => true,
        };
        carrier
            && payload
            && t.family != Family::None
            && r.family == t.family
            && (r.family != Family::OpenAi || r.provider.as_deref().is_none_or(|p| p == t.provider))
    }

    /// Reasoning blocks that are not replayed are reported as ignored; returns whether `b` stays.
    pub(super) fn keep_block(&mut self, b: &Block) -> bool {
        match b {
            Block::Reasoning(r) if !self.replayable(r) => {
                if !r.restored {
                    self.warn(Kind::Ignored, &r.src);
                }
                false
            }
            _ => true,
        }
    }

    /// A cache marker the target receives (after the breakpoint limit), with its ttl.
    pub(super) fn cache_value(&self, c: Option<&super::ir::Cache>) -> Option<Value> {
        let c = c?;
        if self.cache_removed.contains(c.src.at()) {
            return None;
        }
        let mut v = c.value.clone();
        if self.cache_downgraded.contains(c.src.at())
            && let Some(o) = v.as_object_mut()
        {
            o.shift_remove("ttl");
        }
        Some(v)
    }

    /// Reports a cache marker the target cannot carry (Chat without the flag, Responses, Gemini).
    pub(super) fn drop_cache(&mut self, c: Option<&super::ir::Cache>) {
        if let Some(c) = c {
            self.warn(Kind::Ignored, &c.src);
        }
    }

    /// Every cache marker of the request in prompt order: tools, system, then messages.
    pub(super) fn markers(&self) -> Vec<&'a super::ir::Cache> {
        let req = self.req;
        let mut out: Vec<&super::ir::Cache> = Vec::new();
        out.extend(req.tools.iter().filter_map(|t| t.cache.as_ref()));
        out.extend(req.system.iter().filter_map(|s| s.cache.as_ref()));
        for turn in &req.turns {
            for b in &turn.blocks {
                out.extend(block_cache(b));
                if let Block::ToolResult(r) = b {
                    out.extend(r.content.iter().filter_map(block_cache));
                }
            }
            out.extend(turn.cache.as_ref());
        }
        out
    }

    /// Applies the breakpoint limit and the one-hour ordering rule to the request's markers.
    pub(super) fn plan_cache(&mut self) {
        let mut markers = self.markers();
        if let Some(auto) = &self.req.auto_cache {
            markers.push(auto);
        }
        let max = self.t.max_breakpoints;
        let excess = markers.len().saturating_sub(max);
        let removed: Vec<&super::ir::Cache> = markers
            .iter()
            .copied()
            .filter(|c| Some(*c) != self.req.auto_cache.as_ref())
            .take(excess)
            .collect();
        for c in removed {
            self.warn(Kind::Dropped, &c.src);
            self.cache_removed.insert(c.src.at().to_owned());
        }
        let mut seen_short = false;
        let mut downgrade = Vec::new();
        for c in markers
            .iter()
            .filter(|c| !self.cache_removed.contains(c.src.at()))
        {
            if c.one_hour() {
                if seen_short {
                    downgrade.push(c.src.clone());
                }
            } else {
                seen_short = true;
            }
        }
        for src in downgrade {
            self.warn(Kind::Approximated, &src);
            self.cache_downgraded.insert(src.at().to_owned());
        }
    }

    /// The tool-call id sent for a client id.
    pub(super) fn id(&mut self, id: &str) -> String {
        self.ids.up(self.t.dialect, id)
    }
}

fn block_cache(b: &Block) -> Option<&super::ir::Cache> {
    match b {
        Block::Text { cache, .. } | Block::Image { cache, .. } | Block::Document { cache, .. } => {
            cache.as_ref()
        }
        Block::ToolCall(c) => c.cache.as_ref(),
        Block::ToolResult(r) => r.cache.as_ref(),
        _ => None,
    }
}

/// Distinct tool names in history, for targets that need a definition for each.
pub(super) fn history_tool_names(req: &Request) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let defined: HashMap<&str, ()> = req.tools.iter().map(|t| (t.name.as_str(), ())).collect();
    for c in req.calls() {
        if !defined.contains_key(c.name.as_str()) && !out.contains(&c.name) {
            out.push(c.name.clone());
        }
    }
    out
}

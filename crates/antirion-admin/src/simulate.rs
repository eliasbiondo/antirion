//! `POST /routes/{name}:simulate`: a stored or candidate route evaluated against the request facts
//! of logged records, with the router's own decision logic, every deployment taken as healthy and
//! nothing sent or changed.

use antirion_core::UtcMicros;
use antirion_identity::rbac::Scope;
use antirion_router::Facts;
use antirion_router::simulate::Simulation;
use antirion_store::logs::{Arg, LogColumn, LogFilter, LogOrder, SortKey};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::error::AdminError;

/// Records evaluated when the request does not say.
const DEFAULT_SAMPLE: u64 = 10_000;
const MAX_SAMPLE: u64 = 100_000;
/// Records read per store page.
const PAGE: u64 = 1_000;

/// The request facts of one logged record, as conditions read them.
struct Recorded<'a> {
    record: &'a Value,
    body: Option<&'a Value>,
    request: Map<String, Value>,
    sample: Option<f64>,
}

impl<'a> Recorded<'a> {
    fn new(record: &'a Value, seed: u64) -> Recorded<'a> {
        let body = record.pointer("/content/request").filter(|b| b.is_object());
        let mut request = Map::new();
        let get = |p: &str| record.pointer(p).cloned().unwrap_or(Value::Null);
        request.insert("endpoint".into(), get("/operation"));
        request.insert("dialect".into(), get("/dialect"));
        request.insert("path".into(), get("/endpoint"));
        request.insert("model".into(), get("/requested_model"));
        request.insert(
            "stream".into(),
            Value::Bool(record.get("stream").is_some_and(|s| !s.is_null())),
        );
        if let Some(n) = record.pointer("/usage/input").and_then(Value::as_u64) {
            request.insert("input_tokens".into(), json!(n));
        }
        if let Some(b) = body {
            let tools = b
                .get("tools")
                .and_then(Value::as_array)
                .is_some_and(|t| !t.is_empty());
            request.insert("has_tools".into(), json!(tools));
            if let Some(m) = b.get("max_tokens").or_else(|| b.get("max_completion_tokens")) {
                request.insert("max_output_tokens".into(), m.clone());
            }
            let text = b.to_string();
            request.insert(
                "has_images".into(),
                json!(
                    text.contains("\"image_url\"")
                        || text.contains("\"input_image\"")
                        || text.contains("\"type\":\"image\"")
                ),
            );
            request.insert("has_audio".into(), json!(text.contains("\"input_audio\"")));
        }
        let sample = record
            .get("request_id")
            .and_then(Value::as_str)
            .map(|id| antirion_router::predicate::sample_point(seed, id));
        Recorded {
            record,
            body,
            request,
            sample,
        }
    }
}

impl Facts for Recorded<'_> {
    fn header(&self, name: &str) -> Option<String> {
        self.record
            .pointer("/request_headers")
            .and_then(|h| h.get(name))
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Array(a) => Some(a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")),
                _ => None,
            })
    }
    fn body(&self) -> Option<&Value> {
        self.body
    }
    fn metadata(&self) -> Option<&Value> {
        self.record.get("metadata").filter(|m| m.is_object())
    }
    fn request(&self, field: &str) -> Option<Value> {
        self.request.get(field).cloned()
    }
    fn identity(&self, field: &str) -> Option<Value> {
        let v = self.record.get(field)?;
        (!v.is_null()).then(|| v.clone())
    }
    fn sample_point(&self) -> Option<f64> {
        self.sample
    }
}

fn invalid(field: &str, message: &str) -> AdminError {
    AdminError::validation(vec![antirion_config::ConfigError::new(
        &antirion_config::DocPath::root().key(field),
        antirion_config::DetailCode::InvalidFormat,
        message,
    )])
}

fn time(body: &Map<String, Value>, field: &str) -> Result<UtcMicros, AdminError> {
    body.get(field)
        .and_then(Value::as_str)
        .and_then(|t| UtcMicros::parse_rfc3339(t).ok())
        .ok_or_else(|| invalid(field, "an RFC 3339 timestamp is required"))
}

/// The record filter: `filters` members as columns, else the route's own records when it exists.
fn filter(
    body: &Map<String, Value>,
    route: &str,
    stored: bool,
    from: UtcMicros,
    to: UtcMicros,
) -> Result<LogFilter, AdminError> {
    let mut f = LogFilter {
        from_us: Some(from.0),
        to_us: Some(to.0),
        ..LogFilter::default()
    };
    match body.get("filters") {
        None | Some(Value::Null) => {
            if stored {
                f.any_of
                    .push((LogColumn::Route, vec![Arg::Text(route.to_owned())]));
            }
        }
        Some(Value::Object(m)) => {
            for (k, v) in m {
                let values: Vec<String> = match v {
                    Value::String(s) => vec![s.clone()],
                    Value::Array(a) => a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
                    _ => return Err(invalid("filters", "each filter is a string or a list of strings")),
                };
                if k == "tag" {
                    f.tags.extend(values);
                    continue;
                }
                let column = match k.as_str() {
                    "model" => LogColumn::Model,
                    "requested_model" => LogColumn::RequestedModel,
                    "route" => LogColumn::Route,
                    "organization" => LogColumn::Organization,
                    "team" => LogColumn::Team,
                    "project" => LogColumn::Project,
                    "key" => LogColumn::Key,
                    "provider" => LogColumn::Provider,
                    "deployment" => LogColumn::Deployment,
                    "operation" => LogColumn::Operation,
                    _ => return Err(invalid("filters", "an unknown filter")),
                };
                f.any_of
                    .push((column, values.into_iter().map(Arg::Text).collect()));
            }
        }
        Some(_) => return Err(invalid("filters", "filters is an object")),
    }
    Ok(f)
}

pub(crate) async fn simulate(call: &Call, route: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    call.require("models:read", &[Scope::Gateway])?;
    let body = call.json_body()?;
    if let Some(k) = body
        .keys()
        .find(|k| !matches!(k.as_str(), "spec" | "from" | "to" | "sample" | "filters"))
    {
        return Err(invalid(k, "unknown member"));
    }
    let (from, to) = (time(&body, "from")?, time(&body, "to")?);
    if to < from {
        return Err(invalid("to", "to is before from"));
    }
    let sample = match body.get("sample") {
        None | Some(Value::Null) => DEFAULT_SAMPLE,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=MAX_SAMPLE).contains(n))
            .ok_or_else(|| invalid("sample", "sample is an integer from 1 to 100000"))?,
    };
    let stored = call.snap.routing.route(route).is_some();
    let compiled = match body.get("spec") {
        Some(spec) if !spec.is_null() => {
            let mut candidate = call.snap.config.doc.clone();
            if let Some(root) = candidate.as_object_mut() {
                let routes = root
                    .entry("routes")
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .ok_or_else(AdminError::internal)?;
                routes.insert(route.to_owned(), spec.clone());
            }
            call.rt
                .check_unread(candidate.clone(), &call.snap.config.doc)
                .map_err(AdminError::validation)?;
            let snap = antirion_router::compile(&candidate).map_err(AdminError::validation)?;
            snap.routes
                .get(route)
                .map(|r| r.tree.clone())
                .ok_or_else(AdminError::internal)?
        }
        _ => call
            .snap
            .routing
            .route(route)
            .map(|r| r.tree.clone())
            .ok_or_else(AdminError::not_found)?,
    };
    let f = filter(&body, route, stored, from, to)?;
    let seed = call.snap.routing.settings.hash_seed;
    let now = call.rt.now();
    let mut sim = Simulation::new(&compiled);
    let mut after = None;
    while sim.evaluated < sample {
        let want = PAGE.min(sample - sim.evaluated);
        let order = LogOrder {
            sort: SortKey::StartedAt,
            desc: true,
            after: after.take(),
            limit: usize::try_from(want).unwrap_or(usize::MAX),
        };
        let page = call
            .rt
            .list_logs(f.clone(), order)
            .await
            .map_err(|_| AdminError::store_unavailable())?;
        let full = u64::try_from(page.len()).unwrap_or(u64::MAX) == want;
        for l in &page {
            let record = antirion_runtime::reqlog::joined(&l.log);
            let facts = Recorded::new(&record, seed);
            let has_content = facts.body.is_some();
            sim.add(&facts, has_content, seed, now);
        }
        after = page.last().map(|l| l.pos.clone());
        if !full || after.is_none() {
            break;
        }
    }
    let rules: Vec<Value> = sim
        .rules()
        .into_iter()
        .map(|r| json!({"path": r.path, "name": r.name, "count": r.count, "unknown": r.unknown}))
        .collect();
    let leaves: Vec<Value> = sim
        .leaves
        .iter()
        .map(|((path, deployment), count)| json!({"path": path, "deployment": deployment, "count": count}))
        .collect();
    Ok(call.json(
        StatusCode::OK,
        &json!({
            "evaluated": sim.evaluated,
            "matched_by_rule": rules,
            "by_leaf": leaves,
            "blocked": sim.blocked,
            "no_route": sim.no_route,
        }),
    ))
}

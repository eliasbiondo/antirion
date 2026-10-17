//! `GET /logs:tail`: new request-log records as server-sent events (`event: log`), filtered like
//! the list, scoped and redacted for the caller, with a `: keepalive` comment every 15 seconds. A
//! node serves a bounded number of tails at once.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use antirion_identity::rbac::AdminPrincipal;
use antirion_runtime::Runtime;
use antirion_runtime::reqlog::analytics::metadata_matches;
use antirion_runtime::reqlog::{TailSlot, Tailed};
use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use http::header::{CACHE_CONTROL, CONTENT_TYPE};
use http::{HeaderValue, Method, StatusCode};
use serde_json::Value;
use tokio::sync::broadcast::error::RecvError;

use crate::call::Call;
use crate::error::AdminError;
use crate::logquery::{self, TAIL_FILTERS};
use crate::logs::{redact, require_some, scope_of};

const KEEPALIVE: Duration = Duration::from_secs(15);

/// The equality filters of a tail: record member and accepted values.
const MEMBERS: &[(&str, &str)] = &[
    ("request_id", "request_id"),
    ("organization", "organization"),
    ("team", "team"),
    ("project", "project"),
    ("key", "key"),
    ("model", "model"),
    ("provider", "provider"),
    ("deployment", "deployment"),
    ("status_code", "status_code"),
    ("code", "code"),
    ("response_path", "response_path"),
    ("end_user", "end_user"),
    ("session_id", "session_id"),
    ("tag", "tags"),
];

#[derive(Debug, Default)]
struct Filter {
    equals: Vec<(&'static str, Vec<String>)>,
    metadata: Vec<(String, String)>,
    from_us: Option<i64>,
    to_us: Option<i64>,
}

fn member_is(record: &Value, member: &str, values: &[String]) -> bool {
    match record.get(member) {
        Some(Value::String(s)) => values.contains(s),
        Some(Value::Number(n)) => values.contains(&n.to_string()),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .any(|i| values.iter().any(|v| v == i)),
        _ => false,
    }
}

impl Filter {
    fn admits(&self, record: &Value) -> bool {
        let started = record
            .get("started_at")
            .and_then(Value::as_str)
            .and_then(|t| antirion_core::UtcMicros::parse_rfc3339(t).ok())
            .map_or(0, |t| t.0);
        self.from_us.is_none_or(|f| started >= f)
            && self.to_us.is_none_or(|t| started < t)
            && self.equals.iter().all(|(m, vs)| member_is(record, m, vs))
            && metadata_matches(record, &self.metadata)
    }
}

/// The event a tail sends for `record`, when the viewer may see it.
fn event(viewer: &AdminPrincipal, filter: &Filter, record: &Value) -> Option<Bytes> {
    let chain = scope_of(record);
    if !viewer.sees(&chain) || !viewer.allows("logs:read", &chain) || !filter.admits(record) {
        return None;
    }
    let mut r = record.clone();
    if !viewer.allows("logs:read_content", &chain) {
        redact(&mut r);
    }
    Some(Bytes::from(format!("event: log\ndata: {r}\n\n")))
}

struct Tail {
    rt: Arc<Runtime>,
    slot: TailSlot,
    viewer: AdminPrincipal,
    filter: Filter,
}

impl Tail {
    /// The next chunk to send: an event, a keepalive, or `None` when the node stops.
    async fn next(&mut self) -> Option<Bytes> {
        loop {
            let got = tokio::select! {
                () = self.rt.shutdown.cancelled() => return None,
                r = tokio::time::timeout(KEEPALIVE, self.slot.receiver.recv()) => r,
            };
            match got {
                Err(_) => return Some(Bytes::from_static(b": keepalive\n\n")),
                Ok(Err(RecvError::Closed)) => return None,
                Ok(Err(RecvError::Lagged(_))) => {}
                Ok(Ok(Tailed { record, .. })) => {
                    if let Some(e) = event(&self.viewer, &self.filter, &record) {
                        return Some(e);
                    }
                }
            }
        }
    }
}

/// `GET /logs:tail`.
pub(crate) fn tail(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    logquery::allow(call, TAIL_FILTERS, &[])?;
    require_some(call, "logs:read")?;
    let mut filter = Filter {
        from_us: logquery::time(call, "from")?,
        to_us: logquery::time(call, "to")?,
        metadata: call
            .query
            .iter()
            .filter(|(k, _)| k.starts_with("metadata."))
            .cloned()
            .collect(),
        ..Filter::default()
    };
    for (param, member) in MEMBERS {
        let vs: Vec<String> = call
            .query
            .iter()
            .filter(|(k, _)| k == param)
            .map(|(_, v)| v.clone())
            .collect();
        if !vs.is_empty() {
            filter.equals.push((member, vs));
        }
    }
    let slot = call.rt.state.request_logs.subscribe().ok_or_else(|| {
        let mut e = AdminError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "This node serves as many live tails as it can; close one and try again.",
        );
        e.retry_after = Some(5);
        e
    })?;
    let tail = Tail {
        rt: Arc::clone(&call.rt),
        slot,
        viewer: call.principal.clone(),
        filter,
    };
    let stream = futures_util::stream::unfold(tail, |mut t| async move {
        t.next().await.map(|chunk| (Ok::<_, Infallible>(chunk), t))
    });
    let mut resp = Response::new(Body::from_stream(stream));
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    resp.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use antirion_core::audit::ActorKind;
    use antirion_identity::rbac::{Binding, Role, Scope};
    use serde_json::json;

    use super::*;

    fn viewer(role: Role, scope: Scope) -> AdminPrincipal {
        AdminPrincipal {
            kind: ActorKind::Token,
            id: "t".into(),
            user: Some("u".into()),
            bindings: vec![Binding { role, scope }],
            read_only: false,
        }
    }

    #[test]
    fn events_are_scoped_filtered_and_redacted() {
        let record = json!({"id": "a", "organization": "acme", "team": "t1", "model": "m",
            "tags": ["x"], "content": {"request": 1}, "attempts": [], "started_at": "2026-09-30T12:00:00Z"});
        let mut f = Filter::default();
        f.equals.push(("model", vec!["m".into(), "n".into()]));
        f.equals.push(("tags", vec!["x".into()]));
        let admin = viewer(Role::GatewayAdmin, Scope::Gateway);
        let e = event(&admin, &f, &record).unwrap();
        assert!(std::str::from_utf8(&e).unwrap().contains("\"request\":1"));
        let org = viewer(Role::OrgAdmin, Scope::Organization("acme".into()));
        let e = event(&org, &f, &record).unwrap();
        assert!(std::str::from_utf8(&e).unwrap().contains("\"content\":null"));
        let other = viewer(Role::OrgAdmin, Scope::Organization("globex".into()));
        assert!(event(&other, &f, &record).is_none());
        f.equals.push(("team", vec!["t2".into()]));
        assert!(event(&admin, &f, &record).is_none());
    }
}

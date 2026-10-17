//! One authenticated Admin API call: who makes it, its query and body, and the helpers every handler
//! shares (parameter checks, permission checks, auditing, responses).

use std::sync::Arc;

use antirion_config::{Format, parse};
use antirion_core::audit::{Actor, ActorKind, AuditEntry, AuditKind, Outcome};
use antirion_core::{RequestId, headers as h};
use antirion_identity::rbac::{AdminPrincipal, Scope};
use antirion_runtime::admin_auth::Session;
use antirion_runtime::{AuditContext, Runtime, Snapshot};
use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use http::header::{CONTENT_TYPE, LOCATION};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use serde_json::Value;

use crate::error::AdminError;

/// How often a waiting write checks the other nodes.
const PROPAGATION_POLL: std::time::Duration = std::time::Duration::from_millis(100);

pub(crate) struct Call {
    pub(crate) rt: Arc<Runtime>,
    pub(crate) snap: Arc<Snapshot>,
    pub(crate) principal: AdminPrincipal,
    /// The session id and session, when the call was authenticated by a session cookie.
    pub(crate) session: Option<(String, Session)>,
    pub(crate) request_id: RequestId,
    pub(crate) source_ip: Option<String>,
    pub(crate) method: Method,
    pub(crate) query: Vec<(String, String)>,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
    /// `x-antirion-change-reason` of a write, decoded.
    pub(crate) reason: Option<String>,
    /// The request path, as the client sent it.
    pub(crate) path: String,
    /// The raw query string.
    pub(crate) raw_query: Option<String>,
    /// The call runs a change request a second principal approved.
    pub(crate) approved: bool,
}

impl Call {
    pub(crate) fn uri_query(&self) -> Option<&str> {
        self.raw_query.as_deref()
    }

    /// Refuses any query parameter outside `allowed`.
    pub(crate) fn allow_params(&self, allowed: &[&str]) -> Result<(), AdminError> {
        match self.query.iter().find(|(k, _)| !allowed.contains(&k.as_str())) {
            Some((k, _)) => Err(AdminError::invalid_parameter(format!(
                "unknown query parameter {k:?}"
            ))),
            None => Ok(()),
        }
    }

    pub(crate) fn param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// A boolean parameter: absent is `default`, anything but `true`/`false` is refused.
    pub(crate) fn bool_param(&self, name: &str, default: bool) -> Result<bool, AdminError> {
        match self.param(name) {
            None => Ok(default),
            Some("true") => Ok(true),
            Some("false") => Ok(false),
            Some(_) => Err(AdminError::invalid_parameter(format!(
                "{name} must be true or false"
            ))),
        }
    }

    /// Validates `?wait=` against `admin.api.max_wait`; standalone nodes are propagated on commit.
    pub(crate) fn check_wait(&self) -> Result<(), AdminError> {
        if let Some(w) = self.param("wait") {
            let d = antirion_config::scalar::parse_duration(w)
                .ok_or_else(|| AdminError::invalid_parameter("wait must be a duration"))?;
            if d > self.rt.statics.max_wait {
                return Err(AdminError::invalid_parameter("wait exceeds admin.api.max_wait"));
            }
        }
        Ok(())
    }

    /// Requires `permission` on a resource whose scope chain is `chain`.
    pub(crate) fn require(&self, permission: &str, chain: &[Scope]) -> Result<(), AdminError> {
        if self.principal.allows(permission, chain) {
            Ok(())
        } else {
            Err(AdminError::forbidden(permission))
        }
    }

    /// Requires `permission` on a key owned by `owner`.
    pub(crate) fn require_owned(
        &self,
        permission: &str,
        chain: &[Scope],
        owner: Option<&str>,
    ) -> Result<(), AdminError> {
        if self.principal.allows_owned(permission, chain, owner) {
            Ok(())
        } else {
            Err(AdminError::forbidden(permission))
        }
    }

    pub(crate) fn require_gateway_admin(&self) -> Result<(), AdminError> {
        if self.principal.is_gateway_admin() {
            Ok(())
        } else {
            Err(AdminError::forbidden("gateway_admin role"))
        }
    }

    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// The body as a JSON (or YAML) document.
    pub(crate) fn document(&self) -> Result<Value, AdminError> {
        let format = Format::from_content_type(self.header("content-type"));
        parse_any(&self.body, format)
    }

    /// The body of a resource write, which must be JSON.
    pub(crate) fn json_body(&self) -> Result<serde_json::Map<String, Value>, AdminError> {
        if self.body.is_empty() {
            return Err(AdminError::malformed(Vec::new()));
        }
        let v: Value = serde_json::from_slice(&self.body).map_err(|e| {
            AdminError::malformed(vec![antirion_config::ConfigError::new(
                &antirion_config::DocPath::root(),
                antirion_config::DetailCode::MalformedDocument,
                e.to_string(),
            )])
        })?;
        match v {
            Value::Object(m) => Ok(m),
            _ => Err(AdminError::malformed(Vec::new())),
        }
    }

    pub(crate) fn actor(&self) -> Actor {
        Actor {
            kind: self.principal.kind,
            id: Some(self.principal.id.clone()),
        }
    }

    pub(crate) fn audit_context(&self, action: &str, resource: &str, status: StatusCode) -> AuditContext {
        AuditContext {
            actor: self.actor(),
            action: action.to_owned(),
            resource: resource.to_owned(),
            request_id: self.request_id.as_str().to_owned(),
            source_ip: self.source_ip.clone(),
            status_code: status.as_u16(),
            reason: self.reason.clone(),
        }
    }

    /// Records an audit entry for a call that does not commit a generation.
    pub(crate) async fn audit(&self, action: &str, resource: &str, outcome: Outcome, status: StatusCode) {
        self.audit_with(action, resource, outcome, status, Value::Null)
            .await;
    }

    /// As [`audit`](Self::audit), with details of the change (never a secret).
    pub(crate) async fn audit_with(
        &self,
        action: &str,
        resource: &str,
        outcome: Outcome,
        status: StatusCode,
        changes: Value,
    ) {
        self.append_audit(action, resource, outcome, status, changes, AuditKind::Write)
            .await;
    }

    /// Records a sensitive read (`kind: read`): content, exports and values revealed once, never
    /// the value itself.
    pub(crate) async fn audit_read(&self, action: &str, resource: &str) {
        self.append_audit(
            action,
            resource,
            Outcome::Success,
            StatusCode::OK,
            Value::Null,
            AuditKind::Read,
        )
        .await;
    }

    /// As [`audit_read`](Self::audit_read), with details of the read.
    pub(crate) async fn audit_read_with(
        &self,
        action: &str,
        resource: &str,
        outcome: Outcome,
        status: StatusCode,
        changes: Value,
    ) {
        self.append_audit(action, resource, outcome, status, changes, AuditKind::Read)
            .await;
    }

    async fn append_audit(
        &self,
        action: &str,
        resource: &str,
        outcome: Outcome,
        status: StatusCode,
        changes: Value,
        kind: AuditKind,
    ) {
        let generation = self.rt.gens.load().generation;
        let entry = AuditEntry {
            id: RequestId::generate().as_str().to_owned(),
            time: self.rt.now().to_rfc3339(),
            actor: self.actor(),
            action: action.to_owned(),
            resource: resource.to_owned(),
            outcome,
            status_code: status.as_u16(),
            generation_before: Some(generation),
            generation_after: Some(generation),
            request_id: self.request_id.as_str().to_owned(),
            source_ip: self.source_ip.clone(),
            kind,
            reason: if kind == AuditKind::Write {
                self.reason.clone()
            } else {
                None
            },
            changes,
        };
        if let Err(e) = self.rt.store.append_audit(entry).await {
            tracing::warn!(error = %e, "an audit entry could not be written");
        }
        if outcome == Outcome::Success && kind == AuditKind::Write {
            self.rt
                .audit_privileged(action, resource, Some(&self.principal.id));
        }
    }

    /// A JSON response with the generation headers of a mutating call.
    pub(crate) fn json(&self, status: StatusCode, body: &Value) -> Response {
        let mut resp = Response::new(Body::from(serde_json::to_vec(body).unwrap_or_default()));
        *resp.status_mut() = status;
        resp.headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        resp
    }

    /// Whether every live, ready node serves `generation`, waiting up to `?wait=` for it.
    pub(crate) async fn propagated(&self, generation: u64) -> bool {
        if self.rt.cluster.is_none() {
            return true;
        }
        let wait = self
            .param("wait")
            .and_then(antirion_config::scalar::parse_duration)
            .unwrap_or_default();
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let (nodes, _) = crate::nodes::live_nodes(self).await;
            if nodes
                .iter()
                .all(|n| !n.ready || n.config_generation >= generation)
            {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(PROPAGATION_POLL).await;
        }
    }

    /// Adds the generation headers of a mutating call, once `?wait=` is over.
    pub(crate) async fn mutated(&self, resp: Response, generation: u64) -> Response {
        let propagated = self.propagated(generation).await;
        self.mutated_as(resp, generation, propagated)
    }

    pub(crate) fn mutated_as(&self, mut resp: Response, generation: u64, propagated: bool) -> Response {
        let h = resp.headers_mut();
        h.insert(
            HeaderName::from_static(h::CONFIG_GENERATION),
            HeaderValue::from(generation),
        );
        h.insert(
            HeaderName::from_static(h::CONFIG_PROPAGATED),
            HeaderValue::from_static(if propagated { "true" } else { "false" }),
        );
        resp
    }

    pub(crate) fn created(&self, mut resp: Response, location: &str) -> Response {
        if let Ok(v) = HeaderValue::from_str(location) {
            resp.headers_mut().insert(LOCATION, v);
        }
        resp
    }

    pub(crate) fn no_content(&self) -> Response {
        let mut resp = Response::new(Body::empty());
        *resp.status_mut() = StatusCode::NO_CONTENT;
        resp
    }
}

/// The most characters a change reason may have.
const MAX_REASON_CHARS: usize = 500;

/// `x-antirion-change-reason`: percent-encoded UTF-8 of at most 500 characters; an empty value is
/// no reason.
pub(crate) fn change_reason(headers: &HeaderMap) -> Result<Option<String>, AdminError> {
    let Some(raw) = headers.get(h::CHANGE_REASON) else {
        return Ok(None);
    };
    let invalid = || {
        AdminError::invalid_parameter(format!(
            "{} must be percent-encoded UTF-8 of at most {MAX_REASON_CHARS} characters",
            h::CHANGE_REASON
        ))
    };
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(invalid)?;
            out.push(hex);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    let text = String::from_utf8(out).map_err(|_| invalid())?;
    if text.chars().count() > MAX_REASON_CHARS || text.chars().any(char::is_control) {
        return Err(invalid());
    }
    let text = text.trim();
    Ok((!text.is_empty()).then(|| text.to_owned()))
}

pub(crate) fn parse_any(bytes: &[u8], format: Format) -> Result<Value, AdminError> {
    parse(bytes, format).map_err(AdminError::malformed)
}

/// The actor of a call that failed authentication.
pub(crate) fn anonymous() -> Actor {
    Actor {
        kind: ActorKind::Operator,
        id: None,
    }
}

//! SCIM 2.0 provisioning (RFC 7643, RFC 7644) on the admin listener under `/scim/v2`: users become
//! human users of the configuration, groups become role bindings through `group_mappings`. Calls
//! carry the SCIM bearer token: the one configured by reference, or the one the gateway generated
//! (`POST /settings/auth:rotate_scim_token`, returned once; the previous one may keep working for an
//! overlap). Also `GET /auth/scim/status`.

mod groups;
mod users;

use std::sync::Arc;

use antirion_core::audit::Outcome;
use antirion_core::{RequestId, UtcMicros};
use antirion_identity::rbac::Scope;
use antirion_runtime::changes::Target;
use antirion_runtime::{CommitError, Mutation, Runtime};
use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{HeaderValue, Method, StatusCode};
use serde_json::{Map, Value, json};
use subtle::ConstantTimeEq;

use crate::call::Call;
use crate::error::AdminError;

const TOKEN_KEY: &str = "scim_token";
const STATUS_KEY: &str = "scim_status";
const PREFIX: &str = "/scim/v2/";
const MAX_OVERLAP_US: i64 = 3_600_000_000;
const ERROR_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:Error";
const LIST_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:ListResponse";
const PATCH_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:PatchOp";

/// Whether `path` is a SCIM endpoint.
pub(crate) fn is_scim(path: &str) -> bool {
    path.starts_with(PREFIX) || path == "/scim/v2"
}

/// A SCIM failure, in the RFC 7644 §3.12 error body.
#[derive(Debug)]
pub(crate) struct ScimError {
    status: StatusCode,
    scim_type: Option<&'static str>,
    detail: String,
}

impl ScimError {
    pub(crate) fn new(
        status: StatusCode,
        scim_type: Option<&'static str>,
        detail: impl Into<String>,
    ) -> ScimError {
        ScimError {
            status,
            scim_type,
            detail: detail.into(),
        }
    }

    pub(crate) fn not_found() -> ScimError {
        ScimError::new(StatusCode::NOT_FOUND, None, "The resource does not exist.")
    }

    pub(crate) fn invalid(detail: impl Into<String>) -> ScimError {
        ScimError::new(StatusCode::BAD_REQUEST, Some("invalidValue"), detail)
    }

    pub(crate) fn unavailable() -> ScimError {
        ScimError::new(StatusCode::SERVICE_UNAVAILABLE, None, "The store is unavailable.")
    }

    fn response(&self) -> Response {
        let mut body = json!({"schemas": [ERROR_SCHEMA], "status": self.status.as_u16().to_string(), "detail": self.detail});
        if let Some(t) = self.scim_type {
            body["scimType"] = json!(t);
        }
        scim_json(self.status, &body)
    }
}

pub(crate) fn no_content() -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::NO_CONTENT;
    resp
}

/// The audit context of a change SCIM makes.
pub(crate) fn audit(action: &str, resource: &str, status: u16) -> antirion_runtime::AuditContext {
    antirion_runtime::AuditContext {
        actor: antirion_core::audit::Actor {
            kind: antirion_core::audit::ActorKind::System,
            id: Some("scim".to_owned()),
        },
        action: action.to_owned(),
        resource: resource.to_owned(),
        request_id: RequestId::generate().as_str().to_owned(),
        source_ip: None,
        status_code: status,
        reason: None,
    }
}

pub(crate) fn scim_json(status: StatusCode, body: &Value) -> Response {
    let mut resp = Response::new(Body::from(serde_json::to_vec(body).unwrap_or_default()));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/scim+json"));
    resp
}

/// One authenticated SCIM request.
pub(crate) struct Scim {
    pub(crate) rt: Arc<Runtime>,
    pub(crate) method: Method,
    pub(crate) query: Vec<(String, String)>,
    pub(crate) body: Bytes,
}

impl Scim {
    pub(crate) fn param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub(crate) fn object(&self) -> Result<Map<String, Value>, ScimError> {
        match serde_json::from_slice::<Value>(&self.body) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err(ScimError::new(
                StatusCode::BAD_REQUEST,
                Some("invalidSyntax"),
                "The body must be a JSON object.",
            )),
        }
    }

    pub(crate) fn now(&self) -> String {
        self.rt.now().to_rfc3339_seconds()
    }

    /// Commits a change of `users.<name>` entries built by `edit` (`None` deletes the user).
    pub(crate) async fn write_users(
        &self,
        changes: Vec<(String, Option<Value>)>,
        action: &str,
    ) -> Result<(), ScimError> {
        if changes.is_empty() {
            return Ok(());
        }
        let resource = format!(
            "users/{}",
            changes
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(",")
        );
        let result = self
            .rt
            .mutate(|state| {
                let mut doc = state.doc.clone();
                let users = doc
                    .as_object_mut()
                    .map(|o| o.entry("users").or_insert_with(|| json!({})))
                    .and_then(Value::as_object_mut)
                    .ok_or(())?;
                for (name, entry) in &changes {
                    match entry {
                        Some(e) => {
                            users.insert(name.clone(), e.clone());
                        }
                        None => {
                            users.remove(name);
                        }
                    }
                }
                Ok::<_, ()>(Mutation {
                    candidate: doc,
                    writer: "api".to_owned(),
                    written: changes
                        .iter()
                        .map(|(n, _)| Target::Resource("users", n.clone()))
                        .collect(),
                    credentials: Vec::new(),
                    meta_edits: Vec::new(),
                    dry_run: false,
                    source: "scim".to_owned(),
                    audit: Some(audit(action, &resource, 200)),
                })
            })
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(CommitError::Invalid(details)) => Err(ScimError::invalid(
                details
                    .first()
                    .map_or_else(|| "The change is not valid.".to_owned(), |d| d.message.clone()),
            )),
            Err(_) => Err(ScimError::unavailable()),
        }
    }
}

/// The generated token's state: the verifier of the current token, when it was made, and the
/// previous one while its overlap lasts.
async fn token_state(rt: &Runtime) -> Value {
    rt.store
        .get_or_init(TOKEN_KEY, b"{}".to_vec())
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}))
}

/// Whether `presented` is the SCIM token now.
async fn token_ok(rt: &Runtime, presented: &str) -> bool {
    let scim = &rt.gens.load().identity.scim;
    if let Some(t) = &scim.token {
        return bool::from(t.as_bytes().ct_eq(presented.as_bytes()));
    }
    let state = token_state(rt).await;
    let current = state.get("verifier").and_then(Value::as_str);
    if current.is_some_and(|v| rt.pepper.verify(presented, v)) {
        return true;
    }
    let previous = state.get("previous");
    let until = previous
        .and_then(|p| p.get("valid_until_us"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    rt.now().0 < until
        && previous
            .and_then(|p| p.get("verifier"))
            .and_then(Value::as_str)
            .is_some_and(|v| rt.pepper.verify(presented, v))
}

async fn note(rt: &Runtime, error: Option<String>) {
    let mut status = rt
        .store
        .get_or_init(STATUS_KEY, b"{}".to_vec())
        .await
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or_else(|| json!({}));
    status["last_request_at"] = json!(rt.now().to_rfc3339_seconds());
    if let Some(e) = error {
        status["last_error"] = json!(e);
    }
    let _ = rt
        .store
        .put_value(STATUS_KEY, status.to_string().into_bytes())
        .await;
}

/// Serves one SCIM request.
pub(crate) async fn handle(
    rt: &Arc<Runtime>,
    method: Method,
    path: &str,
    bearer: Option<&str>,
    query: Vec<(String, String)>,
    body: Bytes,
) -> Response {
    if !rt.gens.load().identity.scim.enabled {
        return ScimError::not_found().response();
    }
    let authorized = match bearer {
        Some(t) => token_ok(rt, t).await,
        None => false,
    };
    if !authorized {
        let mut resp = ScimError::new(
            StatusCode::UNAUTHORIZED,
            None,
            "A valid SCIM bearer token is required.",
        )
        .response();
        resp.headers_mut()
            .insert(http::header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return resp;
    }
    let s = Scim {
        rt: Arc::clone(rt),
        method,
        query,
        body,
    };
    let rest = path.trim_start_matches("/scim/v2").trim_matches('/');
    let segs: Vec<&str> = rest.split('/').filter(|x| !x.is_empty()).collect();
    let result = match segs.as_slice() {
        ["Users"] => users::collection(&s).await,
        ["Users", id] => users::one(&s, id).await,
        ["Groups"] => groups::collection(&s).await,
        ["Groups", id] => groups::one(&s, id).await,
        ["ServiceProviderConfig"] => Ok(scim_json(StatusCode::OK, &service_provider_config())),
        _ => Err(ScimError::not_found()),
    };
    match result {
        Ok(resp) => {
            note(rt, None).await;
            resp
        }
        Err(e) => {
            note(rt, (e.status.as_u16() >= 400).then(|| e.detail.clone())).await;
            e.response()
        }
    }
}

fn service_provider_config() -> Value {
    json!({
        "schemas": ["urn:ietf:params:scim:schemas:core:2.0:ServiceProviderConfig"],
        "patch": {"supported": true},
        "bulk": {"supported": false, "maxOperations": 0, "maxPayloadSize": 0},
        "filter": {"supported": true, "maxResults": 200},
        "changePassword": {"supported": false},
        "sort": {"supported": false},
        "etag": {"supported": false},
        "authenticationSchemes": [{"type": "oauthbearertoken", "name": "Bearer token", "description": "The SCIM token."}],
    })
}

/// A list response of `resources` (already filtered), paged by `startIndex` and `count`.
pub(crate) fn list(s: &Scim, resources: Vec<Value>) -> Response {
    let start = s
        .param("startIndex")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    let count = s
        .param("count")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100)
        .min(200);
    let total = resources.len();
    let page: Vec<Value> = resources.into_iter().skip(start - 1).take(count).collect();
    scim_json(
        StatusCode::OK,
        &json!({"schemas": [LIST_SCHEMA], "totalResults": total, "startIndex": start,
                "itemsPerPage": page.len(), "Resources": page}),
    )
}

/// A `filter` of the form `<attribute> eq "<value>"`, the one IdPs send.
pub(crate) fn eq_filter(s: &Scim) -> Result<Option<(String, String)>, ScimError> {
    let Some(f) = s.param("filter") else {
        return Ok(None);
    };
    let mut parts = f.splitn(3, ' ');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(attr), Some(op), Some(value)) if op.eq_ignore_ascii_case("eq") => {
            Ok(Some((attr.to_owned(), value.trim().trim_matches('"').to_owned())))
        }
        _ => Err(ScimError::new(
            StatusCode::BAD_REQUEST,
            Some("invalidFilter"),
            "Only `<attribute> eq \"<value>\"` filters are supported.",
        )),
    }
}

/// The operations of a PATCH body.
pub(crate) fn patch_ops(m: &Map<String, Value>) -> Result<Vec<(String, Option<String>, Value)>, ScimError> {
    let schemas_ok = m
        .get("schemas")
        .and_then(Value::as_array)
        .is_some_and(|s| s.iter().any(|x| x == PATCH_SCHEMA));
    let ops = m.get("Operations").and_then(Value::as_array);
    match (schemas_ok, ops) {
        (true, Some(ops)) => ops
            .iter()
            .map(|op| {
                let kind = op
                    .get("op")
                    .and_then(Value::as_str)
                    .map(str::to_ascii_lowercase)
                    .filter(|o| matches!(o.as_str(), "add" | "remove" | "replace"))
                    .ok_or_else(|| ScimError::invalid("op must be add, remove or replace"))?;
                let path = op.get("path").and_then(Value::as_str).map(str::to_owned);
                Ok((kind, path, op.get("value").cloned().unwrap_or(Value::Null)))
            })
            .collect(),
        _ => Err(ScimError::new(
            StatusCode::BAD_REQUEST,
            Some("invalidSyntax"),
            "A PATCH needs the PatchOp schema and Operations.",
        )),
    }
}

pub(crate) fn random_id() -> Result<String, ScimError> {
    antirion_identity::verifier::random_id()
        .map(|id| id.chars().filter(char::is_ascii_alphanumeric).take(24).collect())
        .map_err(|_| ScimError::unavailable())
}

/// `GET /auth/scim/status`.
pub(crate) async fn status(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    call.require("users:read", &[Scope::Gateway])?;
    let store = &call.rt.store;
    let users = store
        .records(users::KIND, None)
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .len();
    let groups = store
        .records(groups::KIND, None)
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .len();
    let st: Value = store
        .get_or_init(STATUS_KEY, b"{}".to_vec())
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}));
    Ok(call.json(
        StatusCode::OK,
        &json!({
            "enabled": call.snap.identity.scim.enabled,
            "users": users,
            "groups": groups,
            "last_request_at": st.get("last_request_at").cloned().unwrap_or(Value::Null),
            "last_error": st.get("last_error").cloned().unwrap_or(Value::Null),
        }),
    ))
}

/// When the generated token was last made, for `GET /settings/auth`.
pub(crate) async fn rotated_at(rt: &Runtime) -> Value {
    token_state(rt)
        .await
        .get("rotated_at")
        .cloned()
        .unwrap_or(Value::Null)
}

/// Replaces the generated SCIM token, the previous one staying valid for `overlap_us`; returns the
/// new secret and, with an overlap, when the previous one stops working.
pub(crate) async fn regenerate(
    rt: &Runtime,
    overlap_us: i64,
) -> Result<(String, Option<UtcMicros>), AdminError> {
    let secret = format!(
        "scim-{}",
        antirion_identity::verifier::random_id().map_err(|_| AdminError::internal())?
    );
    let now = rt.now();
    let old = token_state(rt).await;
    let previous_until = (overlap_us > 0)
        .then(|| old.get("verifier").cloned())
        .flatten()
        .map(|v| (v, UtcMicros(now.0.saturating_add(overlap_us))));
    let state = json!({
        "verifier": rt.pepper.verifier(&secret),
        "rotated_at": now.to_rfc3339_seconds(),
        "previous": previous_until.as_ref().map(|(v, until)| json!({"verifier": v, "valid_until_us": until.0})),
    });
    rt.store
        .put_value(TOKEN_KEY, state.to_string().into_bytes())
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    Ok((secret, previous_until.map(|(_, until)| until)))
}

/// `POST /settings/auth:rotate_scim_token`.
pub(crate) async fn rotate(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    let resource = "settings/auth";
    call.require("security:write", &[Scope::Gateway])
        .map_err(|e| e.with_resource(resource))?;
    let overlap_us = if call.body.is_empty() {
        0
    } else {
        let m = call.json_body()?;
        if let Some(k) = m.keys().find(|k| *k != "overlap") {
            return Err(AdminError::invalid_parameter(format!("unknown member {k:?}")));
        }
        match m.get("overlap") {
            None | Some(Value::Null) => 0,
            Some(Value::String(d)) => antirion_config::scalar::parse_duration(d)
                .and_then(|d| i64::try_from(d.as_micros()).ok())
                .filter(|us| *us <= MAX_OVERLAP_US)
                .ok_or_else(|| AdminError::invalid_parameter("overlap must be a duration of at most 1h"))?,
            Some(_) => return Err(AdminError::invalid_parameter("overlap must be a duration")),
        }
    };
    if call.snap.identity.scim.token.is_some() {
        return Err(AdminError::conflict(
            "conflict",
            "The SCIM token is configured by reference; change the reference to rotate it.",
            Vec::new(),
        ));
    }
    let (secret, previous_until) = regenerate(&call.rt, overlap_us).await?;
    call.audit(
        "settings.auth.rotate_scim_token",
        resource,
        Outcome::Success,
        StatusCode::OK,
    )
    .await;
    call.audit_read("settings.auth.scim_token_revealed", resource)
        .await;
    Ok(call.json(
        StatusCode::OK,
        &json!({
            "secret": secret,
            "previous_valid_until": previous_until.map(UtcMicros::to_rfc3339_seconds),
        }),
    ))
}

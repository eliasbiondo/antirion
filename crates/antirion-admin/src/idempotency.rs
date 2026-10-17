//! `Idempotency-Key` on Admin API `POST`s: one execution per key and operator within
//! `admin.api.idempotency_ttl`. The first call claims the key in the store (shared by every node),
//! runs, and keeps its status, headers and body sealed with the at-rest key (a created key's
//! plaintext is in it); a repeat with the same request gets that answer again, one with another
//! request 422 `idempotency_key_reused`. A claim whose call never finished (a crashed node) lapses
//! after `PENDING_FOR`, so the key can be used again.

use std::future::Future;
use std::time::Duration;

use antirion_store::IdentityRecord;
use axum::body::Body;
use axum::response::Response;
use http::{HeaderName, HeaderValue, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::call::Call;
use crate::error::AdminError;

const KIND: &str = "idempotency";
const HEADER: &str = "idempotency-key";
const DEFAULT_TTL: Duration = Duration::from_secs(24 * 3600);
/// How long a claim holds without an answer.
const PENDING_FOR: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(50);
/// The largest answer kept; Admin API answers to a `POST` are far smaller.
const MAX_KEPT: usize = 64 << 20;
/// Headers of the first answer that a replay does not repeat (the router adds its own).
const NOT_KEPT: &[&str] = &["x-request-id", "set-cookie", "content-length"];

/// The key of a `POST`, when it carries one; a malformed key is refused.
pub(crate) fn key(call: &Call) -> Result<Option<String>, AdminError> {
    if call.method != http::Method::POST {
        return Ok(None);
    }
    let Some(raw) = call.headers.get(HEADER) else {
        return Ok(None);
    };
    match raw.to_str() {
        Ok(k) if valid(k.as_bytes()) => Ok(Some(k.to_owned())),
        _ => Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            "invalid_parameter",
            "Idempotency-Key must be 1 to 255 visible ASCII characters.",
        )),
    }
}

fn valid(raw: &[u8]) -> bool {
    (1..=255).contains(&raw.len()) && raw.iter().all(|b| (0x21..=0x7e).contains(b))
}

fn ttl(call: &Call) -> Duration {
    call.rt
        .statics
        .doc
        .pointer("/admin/api/idempotency_ttl")
        .and_then(Value::as_str)
        .and_then(antirion_config::scalar::parse_duration)
        .unwrap_or(DEFAULT_TTL)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The operator a key belongs to: the user behind the credential, else the credential.
fn operator(call: &Call) -> &str {
    call.principal.user.as_deref().unwrap_or(&call.principal.id)
}

/// What makes two calls the same request: the path, the query and the body.
fn fingerprint(path: &str, query: &[(String, String)], body: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(path.as_bytes());
    for (k, v) in query {
        h.update([0]);
        h.update(k.as_bytes());
        h.update([1]);
        h.update(v.as_bytes());
    }
    h.update([2]);
    h.update(body);
    hex(&h.finalize())
}

fn record_id(call: &Call, key: &str) -> String {
    let mut h = Sha256::new();
    h.update(operator(call).as_bytes());
    h.update([0]);
    h.update(key.as_bytes());
    hex(&h.finalize())
}

fn micros(d: Duration) -> i64 {
    i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
}

fn reused() -> AdminError {
    AdminError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_reused",
        "This Idempotency-Key was used for a different request.",
    )
}

/// Runs `exec` at most once for the call's key, or answers what the first run answered.
pub(crate) async fn run<F, Fut>(call: &Call, path: &str, key: &str, exec: F) -> Result<Response, AdminError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Response, AdminError>>,
{
    let id = record_id(call, key);
    let fp = fingerprint(path, &call.query, &call.body);
    let store = &call.rt.store;
    let unavailable = |e: antirion_store::StoreError| {
        tracing::warn!(error = %e, "an idempotency record could not be read or written");
        AdminError::store_unavailable()
    };
    let waited_until = tokio::time::Instant::now() + PENDING_FOR;
    loop {
        let now = call.rt.now().0;
        let claim = IdentityRecord {
            kind: KIND.into(),
            id: id.clone(),
            user: operator(call).to_owned(),
            expires_us: Some(now.saturating_add(micros(PENDING_FOR))),
            body: json!({"state": "pending", "fp": fp}).to_string(),
        };
        if store.claim_record(claim, now).await.map_err(unavailable)? {
            if let Err(e) = store.purge_expired(KIND, now).await {
                tracing::debug!(error = %e, "expired idempotency records were not purged");
            }
            return first_run(call, &id, &fp, exec).await;
        }
        let Some(held) = store.record(KIND, &id).await.map_err(unavailable)? else {
            continue;
        };
        let held: Value = serde_json::from_str(&held.body).unwrap_or(Value::Null);
        if held.get("fp").and_then(Value::as_str) != Some(fp.as_str()) {
            return Err(reused());
        }
        if held.get("state").and_then(Value::as_str) == Some("done") {
            return replay(call, &id, &held);
        }
        if tokio::time::Instant::now() >= waited_until {
            return Err(AdminError::store_unavailable());
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn first_run<F, Fut>(call: &Call, id: &str, fp: &str, exec: F) -> Result<Response, AdminError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Response, AdminError>>,
{
    let resp = match exec().await {
        Ok(r) => r,
        Err(e) if e.status.is_server_error() || matches!(e.status.as_u16(), 401 | 403) => {
            forget(call, id).await;
            return Err(e);
        }
        Err(e) => e.into_response(&call.request_id),
    };
    if resp.status().is_server_error() {
        forget(call, id).await;
        return Ok(resp);
    }
    let (parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_KEPT).await else {
        forget(call, id).await;
        return Err(AdminError::internal());
    };
    let headers: Vec<Value> = parts
        .headers
        .iter()
        .filter(|(k, _)| !NOT_KEPT.contains(&k.as_str()))
        .filter_map(|(k, v)| Some(json!([k.as_str(), v.to_str().ok()?])))
        .collect();
    let sealed = call.rt.sealer.seal(&location(id), &bytes);
    let now = call.rt.now().0;
    let kept = sealed.map(|body| IdentityRecord {
        kind: KIND.into(),
        id: id.to_owned(),
        user: operator(call).to_owned(),
        expires_us: Some(now.saturating_add(micros(ttl(call)))),
        body: json!({"state": "done", "fp": fp, "status": parts.status.as_u16(),
                     "headers": headers, "body": body})
        .to_string(),
    });
    match kept {
        Ok(rec) => {
            if let Err(e) = call.rt.store.put_record(rec).await {
                tracing::warn!(error = %e, "an idempotent answer could not be kept");
            }
        }
        Err(e) => tracing::warn!(error = %e, "an idempotent answer could not be sealed"),
    }
    Ok(Response::from_parts(parts, Body::from(bytes)))
}

fn location(id: &str) -> String {
    format!("{KIND}/{id}")
}

/// Releases a claim whose call should be repeatable (a server error, a refused credential).
async fn forget(call: &Call, id: &str) {
    if let Err(e) = call.rt.store.delete_record(KIND, id).await {
        tracing::warn!(error = %e, "an idempotency claim could not be released");
    }
}

fn replay(call: &Call, id: &str, held: &Value) -> Result<Response, AdminError> {
    let body = held
        .get("body")
        .and_then(Value::as_str)
        .and_then(|b| call.rt.sealer.open(&location(id), b).ok())
        .ok_or_else(AdminError::internal)?;
    let status = held
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|s| u16::try_from(s).ok())
        .and_then(|s| StatusCode::from_u16(s).ok())
        .ok_or_else(AdminError::internal)?;
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = status;
    for pair in held
        .get("headers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(k), Some(v)) = (
            pair.get(0).and_then(Value::as_str),
            pair.get(1).and_then(Value::as_str),
        ) else {
            continue;
        };
        if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            resp.headers_mut().append(k, v);
        }
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_visible_ascii_up_to_255() {
        assert!(valid(b"create-key-1"));
        assert!(valid(&[b'a'; 255]));
        assert!(!valid(&[b'a'; 256]));
        assert!(!valid(b""));
        assert!(!valid(b"with space"));
        assert!(!valid("caf\u{e9}".as_bytes()));
    }

    #[test]
    fn the_same_request_has_the_same_fingerprint() {
        let q = vec![("wait".to_owned(), "1s".to_owned())];
        let a = fingerprint("/admin/v1/keys", &q, b"{}");
        assert_eq!(a, fingerprint("/admin/v1/keys", &q, b"{}"));
        assert_ne!(a, fingerprint("/admin/v1/keys", &q, b"{ }"));
        assert_ne!(a, fingerprint("/admin/v1/keys", &[], b"{}"));
        assert_ne!(a, fingerprint("/admin/v1/routes", &q, b"{}"));
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
    }
}

//! Client-supplied media URLs fetched by the gateway, for deployments that accept only inline data:
//! a client-influenced egress with no client headers, bounded in time and size, and only the media
//! types the operator allows.

use std::sync::Arc;
use std::time::Duration;

use antirion_config::scalar::{parse_duration, parse_size};
use antirion_core::Stage;
use antirion_egress::{OutboundRequest, TransportError, TrustClass};
use antirion_error::{GatewayError, Message};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, Method, Uri};
use http_body_util::BodyExt;
use serde_json::Value;

use antirion_dialects::xlate::encode::{Fetched, MediaMap, Need};

use crate::ctx::Ctx;
use crate::reasons;

/// `security.media_fetch`.
struct Settings {
    max_bytes: u64,
    connect_timeout: Duration,
    timeout: Duration,
    images: Vec<String>,
    documents: Vec<String>,
}

impl Settings {
    fn of(doc: &Value) -> Settings {
        let m = doc.pointer("/security/media_fetch");
        let get = |k: &str| m.and_then(|m| m.get(k)).and_then(Value::as_str);
        let types = |k: &str, default: &[&str]| -> Vec<String> {
            m.and_then(|m| m.pointer(&format!("/allowed_types/{k}")))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_ascii_lowercase)
                        .collect()
                })
                .unwrap_or_else(|| default.iter().map(|s| (*s).to_owned()).collect())
        };
        Settings {
            max_bytes: get("max_bytes").and_then(parse_size).unwrap_or(20 << 20),
            connect_timeout: get("connect_timeout")
                .and_then(parse_duration)
                .unwrap_or(Duration::from_secs(5)),
            timeout: get("timeout")
                .and_then(parse_duration)
                .unwrap_or(Duration::from_secs(30)),
            images: types("image", &["image/png", "image/jpeg", "image/gif", "image/webp"]),
            documents: types("document", &["application/pdf"]),
        }
    }
}

fn refused(reason: antirion_error::Reason, message: &'static str, param: &str) -> GatewayError {
    GatewayError::with_reason(reason, Stage::Attempt, Message::Fixed(message)).param(param.to_owned())
}

fn type_allowed(allowed: &[String], essence: &str) -> bool {
    allowed.iter().any(|a| match a.strip_suffix("/*") {
        Some(prefix) => essence.split('/').next() == Some(prefix),
        None => a == essence,
    })
}

/// Fetches one URL: its media type and bytes.
async fn fetch(
    cx: &Ctx,
    s: &Settings,
    url: &str,
    allowed: &[String],
    param: &str,
    budget: u64,
) -> Result<(String, Bytes), GatewayError> {
    let not_allowed = || refused(reasons::URL_NOT_ALLOWED, "The URL is not allowed.", param);
    let failed = || {
        refused(
            reasons::MEDIA_FETCH_FAILED,
            "The media URL could not be fetched.",
            param,
        )
    };
    let policy = Arc::clone(&cx.snap.security.egress);
    antirion_egress::ssrf::validate_url(url, TrustClass::ClientInfluenced, &policy)
        .map_err(|_| not_allowed())?;
    let uri: Uri = url.parse().map_err(|_| not_allowed())?;
    let tls = cx.snap.security.egress_tls.clone().ok_or_else(failed)?;
    let request = OutboundRequest {
        method: Method::GET,
        uri,
        headers: HeaderMap::new(),
        body: Bytes::new().into(),
        response_timeout: s.timeout.max(s.connect_timeout),
        connect_timeout: None,
        fresh_connection: false,
        trust: TrustClass::ClientInfluenced,
        tls,
        policy,
        resend_when_closed: false,
    };
    let work = async {
        let resp = match cx.rt.egress.send(request).await {
            Ok(r) => r,
            Err(TransportError::EgressDenied(_) | TransportError::UnsupportedScheme(_)) => {
                return Err(not_allowed());
            }
            Err(_) => return Err(failed()),
        };
        let status = resp.status.as_u16();
        if (300..400).contains(&status) {
            return Err(refused(
                reasons::MEDIA_REDIRECT,
                "Redirects are not followed; use the final URL directly.",
                param,
            ));
        }
        if !(200..300).contains(&status) {
            return Err(failed());
        }
        let essence = resp
            .headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(|v| v.trim().to_ascii_lowercase())
            .unwrap_or_default();
        if !type_allowed(allowed, &essence) {
            return Err(refused(
                reasons::MEDIA_TYPE_NOT_ALLOWED,
                "The media type is not allowed.",
                param,
            ));
        }
        let mut body = resp.body.into_data_stream();
        let mut out = BytesMut::new();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|_| failed())?;
            if u64::try_from(out.len() + chunk.len()).unwrap_or(u64::MAX) > s.max_bytes.min(budget) {
                drop(body);
                if let Some(abort) = &resp.abort {
                    Box::pin(abort.abort()).await;
                }
                return Err(refused(
                    reasons::MEDIA_TOO_LARGE,
                    "The media is larger than allowed.",
                    param,
                ));
            }
            out.extend_from_slice(&chunk);
        }
        Ok((essence, out.freeze()))
    };
    tokio::time::timeout(s.timeout, work)
        .await
        .map_err(|_| failed())?
}

/// Whether a URL carries userinfo (`user:pass@` before the host).
fn has_userinfo(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    authority.contains('@')
}

/// Whether a member holds a URL: `url`, `image_url`, `fileUri` and the like.
fn url_member(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with("url") || n.ends_with("uri")
}

/// Looks for a URL member with userinfo; `path` ends at the member found.
fn find_userinfo(v: &Value, path: &mut String) -> bool {
    match v {
        Value::String(s) => path.rsplit(['.', '[']).next().is_some_and(url_member) && has_userinfo(s),
        Value::Array(items) => items.iter().enumerate().any(|(i, item)| {
            let len = path.len();
            path.push_str(&format!("[{i}]"));
            let found = find_userinfo(item, path);
            if !found {
                path.truncate(len);
            }
            found
        }),
        Value::Object(map) => map.iter().any(|(k, item)| {
            let len = path.len();
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(k);
            let found = find_userinfo(item, path);
            if !found {
                path.truncate(len);
            }
            found
        }),
        _ => false,
    }
}

/// Refuses a request whose body carries a URL with userinfo: a credential the gateway would otherwise
/// hand to a resolver or an upstream.
pub(crate) fn refuse_userinfo(body: Option<&Value>) -> Result<(), GatewayError> {
    let Some(body) = body else {
        return Ok(());
    };
    let mut path = String::new();
    if find_userinfo(body, &mut path) {
        return Err(GatewayError::with_reason(
            reasons::URL_NOT_ALLOWED,
            Stage::Parse,
            Message::Fixed("The URL is not allowed."),
        )
        .param(path));
    }
    Ok(())
}

/// `api.media_fetch`: the per-request caps.
fn request_caps(doc: &Value) -> (usize, u64) {
    let m = doc.pointer("/api/media_fetch");
    let objects = m
        .and_then(|m| m.get("max_objects"))
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(20);
    let total = m
        .and_then(|m| m.get("max_total_bytes"))
        .and_then(Value::as_str)
        .and_then(parse_size)
        .unwrap_or(50 << 20);
    (objects, total)
}

/// Whether media fetching is enabled (`api.media_fetch.enabled`).
pub(crate) fn enabled(doc: &Value) -> bool {
    doc.pointer("/api/media_fetch/enabled").and_then(Value::as_bool) != Some(false)
}

/// What one request fetched so far: identical URLs are fetched once and reused across attempts.
#[derive(Debug, Default)]
pub(crate) struct Fetches {
    pub(crate) map: MediaMap,
    bytes: u64,
}

/// Fetches the URLs a target needs that the request has not fetched yet, within the per-request
/// caps.
pub(crate) async fn fetch_needs(cx: &Ctx, needs: &[Need], done: &mut Fetches) -> Result<(), GatewayError> {
    let s = Settings::of(&cx.snap.config.doc);
    let (max_objects, max_total) = request_caps(&cx.snap.config.doc);
    let missing: Vec<&Need> = needs.iter().filter(|n| !done.map.contains_key(&n.url)).collect();
    if done.map.len() + missing.len() > max_objects {
        let param = missing.first().map(|n| n.param.clone()).unwrap_or_default();
        return Err(refused(
            reasons::MEDIA_TOO_MANY,
            "The request needs more remote media fetches than allowed.",
            &param,
        ));
    }
    for n in missing {
        let allowed = if n.image { &s.images } else { &s.documents };
        let budget = max_total.saturating_sub(done.bytes);
        let (mime, bytes) = fetch(cx, &s, &n.url, allowed, &n.param, budget).await?;
        done.bytes += u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        done.map.insert(
            n.url.clone(),
            Fetched {
                mime,
                data: STANDARD.encode(&bytes),
            },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_types() {
        let allowed = vec!["image/png".to_owned(), "audio/*".to_owned()];
        assert!(type_allowed(&allowed, "image/png"));
        assert!(type_allowed(&allowed, "audio/wav"));
        assert!(!type_allowed(&allowed, "text/html"));
        let s = Settings::of(
            &serde_json::json!({"security": {"media_fetch": {"max_bytes": "1KiB", "timeout": "2s"}}}),
        );
        assert_eq!((s.max_bytes, s.timeout), (1024, Duration::from_secs(2)));
        assert_eq!(s.images.len(), 4);
    }

    #[test]
    fn userinfo_is_found_with_its_path() {
        let body = serde_json::json!({"messages": [{"content": [{"type": "text", "text": "https://u:p@h.example/"},
            {"type": "image_url", "image_url": {"url": "https://k:s@h.example/x.png"}}]}]});
        let err = refuse_userinfo(Some(&body)).unwrap_err();
        assert_eq!(err.param.as_deref(), Some("messages[0].content[1].image_url.url"));
        assert!(!has_userinfo("https://h.example/a@b"));
        assert!(!has_userinfo("mailto:a@b"));
    }
}

//! What Wasm extensions need from the node: the epoch clock their time limits count in, advanced
//! only while the serving configuration has a Wasm extension, and the fetch of `oci://` modules
//! through the egress client (operator trust) before a configuration naming them is compiled.

use std::sync::Arc;
use std::time::Duration;

use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_egress::{OutboundRequest, TrustClass};
use antirion_ext::wasm::OciRef;
use bytes::Bytes;
use futures_util::StreamExt;
use http::header::{ACCEPT, AUTHORIZATION, WWW_AUTHENTICATE};
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use http_body_util::BodyExt;
use serde_json::Value;

use crate::Runtime;
use crate::security::SecurityRt;

/// How often the clock checks whether a Wasm extension is configured.
const IDLE: Duration = Duration::from_millis(100);
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// The largest module fetched.
const MAX_MODULE: usize = 256 << 20;

/// Advances the Wasm epoch every tick while a Wasm extension is configured, until shutdown.
pub async fn clock(rt: Arc<Runtime>) {
    loop {
        let active = rt.gens.load().plugins.has_wasm();
        let wait = if active { antirion_ext::wasm::TICK } else { IDLE };
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
        if active {
            antirion_ext::wasm::tick();
        }
    }
}

/// The `oci://` modules of a document's Wasm extensions that are not fetched yet: path, reference
/// and digest.
fn wanted(doc: &Value) -> Vec<(DocPath, OciRef, String)> {
    let root = DocPath::root().key("plugins");
    doc.get("plugins")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, p)| p.get("type").and_then(Value::as_str) == Some("wasm"))
        .filter_map(|(name, p)| {
            let w = p.get("wasm")?;
            let module = w.get("module").and_then(Value::as_str)?;
            let sha256 = w.get("sha256").and_then(Value::as_str)?.to_ascii_lowercase();
            let at = root.key(name).key("wasm").key("module");
            if antirion_ext::wasm::fetched(&sha256) || !module.starts_with("oci://") {
                return None;
            }
            Some((at, OciRef::parse(module)?, sha256))
        })
        .collect()
}

/// The `realm`, `service` and `scope` of a `Bearer` challenge.
fn challenge(h: &HeaderMap) -> Option<String> {
    let v = h.get(WWW_AUTHENTICATE)?.to_str().ok()?;
    let params = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer "))?;
    let mut realm = None;
    let mut query = Vec::new();
    for part in params.split(',') {
        let (k, v) = part.trim().split_once('=')?;
        let v = v.trim_matches('"');
        match k {
            "realm" => realm = Some(v.to_owned()),
            "service" | "scope" => query.push(format!("{k}={v}")),
            _ => {}
        }
    }
    let realm = realm?;
    Some(if query.is_empty() {
        realm
    } else {
        format!("{realm}?{}", query.join("&"))
    })
}

impl Runtime {
    async fn get(
        &self,
        sec: &SecurityRt,
        uri: Uri,
        headers: HeaderMap,
    ) -> Result<(StatusCode, HeaderMap, Vec<u8>), String> {
        let tls = sec.egress_tls.clone().ok_or("no egress TLS profile")?;
        let resp = self
            .egress
            .send(OutboundRequest {
                method: Method::GET,
                uri,
                headers,
                body: Bytes::new().into(),
                response_timeout: FETCH_TIMEOUT,
                connect_timeout: Some(FETCH_TIMEOUT),
                fresh_connection: false,
                trust: TrustClass::Operator,
                tls,
                policy: Arc::clone(&sec.egress),
                resend_when_closed: true,
            })
            .await
            .map_err(|e| e.to_string())?;
        let (status, head) = (resp.status, resp.headers.clone());
        let mut chunks = resp.body.into_data_stream();
        let mut out = Vec::new();
        while let Some(chunk) = chunks.next().await {
            out.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
            if out.len() > MAX_MODULE {
                return Err("the module is larger than 256 MiB".to_owned());
            }
        }
        Ok((status, head, out))
    }

    /// One blob, with the registry's anonymous token when it asks for one.
    async fn fetch_blob(&self, sec: &SecurityRt, r: &OciRef, sha256: &str) -> Result<Vec<u8>, String> {
        let uri: Uri = r
            .blob_url(sha256)
            .parse()
            .map_err(|_| "the registry URL is not valid".to_owned())?;
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/octet-stream, */*"));
        let (status, head, body) = self.get(sec, uri.clone(), headers.clone()).await?;
        let body = if status == StatusCode::UNAUTHORIZED {
            let realm =
                challenge(&head).ok_or("the registry asked for credentials the gateway does not have")?;
            let realm: Uri = realm
                .parse()
                .map_err(|_| "the registry's token URL is not valid".to_owned())?;
            let (status, _, token) = self.get(sec, realm, HeaderMap::new()).await?;
            if !status.is_success() {
                return Err(format!("the registry's token endpoint answered {status}"));
            }
            let token: Value =
                serde_json::from_slice(&token).map_err(|_| "the token answer is not JSON".to_owned())?;
            let token = token
                .get("token")
                .or_else(|| token.get("access_token"))
                .and_then(Value::as_str)
                .ok_or("the token answer has no token")?;
            let bearer = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| "the token is not a header value".to_owned())?;
            headers.insert(AUTHORIZATION, bearer);
            let (status, _, body) = self.get(sec, uri, headers).await?;
            if !status.is_success() {
                return Err(format!("the registry answered {status}"));
            }
            body
        } else if status.is_success() {
            body
        } else {
            return Err(format!("the registry answered {status}"));
        };
        Ok(body)
    }

    /// Fetches the `oci://` modules `doc` names that are not fetched yet, under the document's own
    /// egress rules (the serving ones when they cannot be compiled).
    pub(crate) async fn fetch_wasm_modules(&self, doc: &Value) -> Vec<ConfigError> {
        let wanted = wanted(doc);
        if wanted.is_empty() {
            return Vec::new();
        }
        let sec = match SecurityRt::compile(doc) {
            Ok(s) => Arc::new(s),
            Err(_) => Arc::clone(&self.gens.load().security),
        };
        let mut errors = Vec::new();
        for (at, r, sha256) in wanted {
            let fetched = self.fetch_blob(&sec, &r, &sha256).await;
            let error = match fetched {
                Ok(bytes) => {
                    if antirion_ext::wasm::provide(&sha256, bytes) {
                        continue;
                    }
                    "the registry's blob does not have the configured sha256".to_owned()
                }
                Err(e) => e,
            };
            tracing::warn!(registry = %r.registry, repository = %r.repository, error = %error, "a Wasm module could not be fetched");
            errors.push(ConfigError::new(
                &at,
                DetailCode::UnresolvedReference,
                format!("the module could not be fetched from {}: {error}", r.registry),
            ));
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn bearer_challenges_name_the_token_url() {
        let mut h = HeaderMap::new();
        h.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Bearer realm="https://auth.example/token",service="registry.example",scope="repository:x:pull""#),
        );
        assert_eq!(
            challenge(&h).as_deref(),
            Some("https://auth.example/token?service=registry.example&scope=repository:x:pull")
        );
        h.insert(WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=x"));
        assert!(challenge(&h).is_none());
    }

    #[test]
    fn only_unfetched_oci_modules_are_wanted() {
        let doc = json!({"plugins": {
            "a": {"type": "wasm", "wasm": {"module": "oci://r.example/a:1", "sha256": "ab".repeat(32)}},
            "b": {"type": "wasm", "wasm": {"module": "file:///b.wasm", "sha256": "cd".repeat(32)}},
            "c": {"type": "webhook", "webhook": {"url": "https://h"}}
        }});
        let w = wanted(&doc);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].0.to_string(), "plugins.a.wasm.module");
        assert_eq!(w[0].1.repository, "a");
    }
}

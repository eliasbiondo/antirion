//! What the record keeps of request metadata (the client's `x-antirion-metadata`, parsed by the
//! controls, bounded; operator key and project metadata win on collision), and the session value
//! from the sources after the session header.

use antirion_runtime::identity::KeyRt;
use http::HeaderMap;
use serde_json::{Map, Value};

use crate::ctx::Ctx;

/// The metadata the record keeps, and how many entries were dropped.
pub(crate) fn recorded(cx: &Ctx, client: Option<&Value>, key: &KeyRt) -> (Map<String, Value>, u64) {
    let limits = cx.snap.obs.metadata;
    let client = client.and_then(Value::as_object).cloned().unwrap_or_default();
    let (mut out, dropped) =
        antirion_obs::record::metadata(&client, limits.max_entries, limits.max_value_bytes);
    let doc = &cx.snap.config.doc;
    for (collection, name) in [("projects", key.project.as_str()), ("keys", key.name.as_str())] {
        let operator = doc
            .get(collection)
            .and_then(|c| c.get(name))
            .and_then(|r| r.get("metadata"))
            .and_then(Value::as_object);
        if let Some(m) = operator {
            let (bounded, _) = antirion_obs::record::metadata(m, usize::MAX, limits.max_value_bytes);
            out.extend(bounded);
        }
    }
    (out, dropped)
}

fn valid_session(v: &str) -> Option<String> {
    let v = v.trim();
    ((1..=256).contains(&v.len()) && v.bytes().all(|b| (0x21..=0x7e).contains(&b))).then(|| v.to_owned())
}

/// The session value from the sources after the session header: body `prompt_cache_key`, header
/// `x-claude-code-session-id`, body `metadata.user_id`.
pub(crate) fn later_session(headers: &HeaderMap, body: Option<&Value>) -> Option<String> {
    body.and_then(|b| b.get("prompt_cache_key"))
        .and_then(Value::as_str)
        .and_then(valid_session)
        .or_else(|| {
            headers
                .get("x-claude-code-session-id")
                .and_then(|v| v.to_str().ok())
                .and_then(valid_session)
        })
        .or_else(|| {
            body.and_then(|b| b.pointer("/metadata/user_id"))
                .and_then(Value::as_str)
                .and_then(valid_session)
        })
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;
    use serde_json::json;

    use super::*;

    #[test]
    fn session_sources_in_order() {
        let mut h = HeaderMap::new();
        assert_eq!(
            later_session(&h, Some(&json!({"prompt_cache_key": "p1"}))).as_deref(),
            Some("p1")
        );
        h.insert("x-claude-code-session-id", HeaderValue::from_static("c1"));
        assert_eq!(later_session(&h, Some(&json!({}))).as_deref(), Some("c1"));
    }
}

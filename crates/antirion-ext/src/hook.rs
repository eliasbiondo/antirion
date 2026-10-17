//! The webhook carrier's version-1 contract: the payload sent for a hook, its signature, and the
//! strict reading of the answer into a [`HookAction`].

use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Map, Value, json};
use sha2::Sha256;

use crate::api::Input;
use crate::patch::touched;
use crate::plugin::{Hook, Plugin};

/// A `respond` answer: a reply rendered in the client's dialect without any upstream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub content: String,
    pub finish_reason: String,
    pub input: u64,
    pub output: u64,
}

/// What a hook decided.
#[derive(Clone, Debug, PartialEq)]
pub enum HookAction {
    Continue,
    Mutate {
        patch: Vec<Value>,
        set_headers: Vec<(String, String)>,
        metadata: Map<String, Value>,
    },
    Deny {
        status: u16,
        message: String,
    },
    Respond(Reply),
}

impl HookAction {
    /// The action name for logs and metrics.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            HookAction::Continue => "continue",
            HookAction::Mutate { .. } => "mutate",
            HookAction::Deny { .. } => "deny",
            HookAction::Respond(_) => "respond",
        }
    }
}

/// The request facts a hook payload carries (never a credential).
#[derive(Clone, Debug, Default)]
pub struct HookFacts {
    pub request_id: String,
    pub log_id: String,
    /// `{organization, team, project, key, end_user, tags, metadata}`.
    pub identity: Value,
    pub model: String,
    pub route: Option<String>,
    pub operation: String,
    pub dialect: String,
    pub stream: bool,
    /// The client headers named by `forward_headers`, all of them; each extension gets its own.
    pub headers: Vec<(String, String)>,
}

/// The JSON payload of one hook call.
#[must_use]
pub fn payload(
    plugin: &Plugin,
    hook: Hook,
    facts: &HookFacts,
    body: Option<&Value>,
    response: Option<(u16, &Value)>,
    forward: &[String],
) -> Vec<u8> {
    let mut input = Input::new(hook, &plugin.name, &plugin.config, facts).with_body(body);
    if let Some((status, r)) = response {
        input = input.with_response(status, r);
    }
    payload_of(&input, plugin.include_body, forward)
}

/// The JSON payload of one hook call; without `include_body`, the bodies and the event's data
/// are left out.
#[must_use]
pub fn payload_of(input: &Input<'_>, include_body: bool, forward: &[String]) -> Vec<u8> {
    let facts = input.facts;
    let headers: Map<String, Value> = facts
        .headers
        .iter()
        .filter(|(k, _)| forward.contains(k))
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    let mut p = json!({
        "version": 1,
        "hook": input.hook.as_str(),
        "extension": input.extension,
        "config": input.config,
        "request_id": facts.request_id,
        "log_id": facts.log_id,
        "identity": facts.identity,
        "model": facts.model,
        "route": facts.route,
        "operation": facts.operation,
        "dialect": facts.dialect,
        "stream": facts.stream,
        "headers": headers,
    });
    if let Some(o) = p.as_object_mut() {
        if include_body && let Some(b) = input.body {
            o.insert("body".into(), b.clone());
        }
        if let Some((status, r)) = input.response {
            o.insert("status".into(), Value::from(status));
            if include_body {
                o.insert("response".into(), r.clone());
            }
        }
        if let Some(e) = input.event {
            let mut event = json!({"index": e.index, "event": e.name});
            if include_body && let Some(ev) = event.as_object_mut() {
                ev.insert("data".into(), e.data.clone());
            }
            o.insert("event".into(), event);
        }
        if let Some(a) = input.attempt {
            o.insert(
                "attempt".into(),
                json!({"index": a.index, "deployment": a.deployment, "provider": a.provider,
                       "provider_type": a.provider_type, "model": a.model}),
            );
        }
    }
    serde_json::to_vec(&p).unwrap_or_default()
}

/// `t=<unix seconds>,v1=<hex HMAC-SHA256(secret, "<t>.<body>")>`.
#[must_use]
pub fn signature(secret: &str, body: &[u8], now_secs: i64) -> Option<String> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(format!("{now_secs}.").as_bytes());
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Some(format!("t={now_secs},v1={hex}"))
}

fn forbidden_path(p: &str) -> bool {
    ["/model", "/stream"]
        .iter()
        .any(|f| p == *f || p.starts_with(&format!("{f}/")))
}

/// Reads a hook answer. Anything outside the contract is a failure.
pub fn parse_answer(hook: Hook, status: u16, body: &[u8]) -> Result<HookAction, String> {
    if !(200..=299).contains(&status) {
        return Err(format!("the extension answered with status {status}"));
    }
    let v: Value = serde_json::from_slice(body).map_err(|_| "the answer is not JSON".to_owned())?;
    let o = v.as_object().ok_or("the answer is not a JSON object")?;
    let action = match o.get("action").and_then(Value::as_str) {
        Some("continue") => HookAction::Continue,
        Some("mutate") => {
            let patch = match o.get("body_patch") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(ops)) => ops.clone(),
                Some(_) => return Err("`body_patch` is not an array".to_owned()),
            };
            let mut set_headers = Vec::new();
            for (k, v) in o
                .get("set_headers")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                let value = v.as_str().ok_or("a header value is not a string")?;
                set_headers.push((k.to_ascii_lowercase(), value.to_owned()));
            }
            let metadata = o
                .get("metadata")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            HookAction::Mutate {
                patch,
                set_headers,
                metadata,
            }
        }
        Some("deny") => {
            let status = o.get("status").and_then(Value::as_u64);
            let status = match status {
                Some(s @ (400 | 403)) => u16::try_from(s).unwrap_or(400),
                _ => return Err("a deny status must be 400 or 403".to_owned()),
            };
            let message = o
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("The request was denied by an extension.")
                .to_owned();
            HookAction::Deny { status, message }
        }
        Some("respond") => {
            let content = o
                .get("content")
                .and_then(Value::as_str)
                .ok_or("a respond answer needs `content`")?
                .to_owned();
            let finish_reason = match o.get("finish_reason").and_then(Value::as_str) {
                None => "stop",
                Some(f @ ("stop" | "length" | "content_filter")) => f,
                Some(_) => return Err("unknown finish_reason".to_owned()),
            }
            .to_owned();
            let usage = o.get("usage");
            let count = |k: &str| usage.and_then(|u| u.get(k)).and_then(Value::as_u64).unwrap_or(0);
            HookAction::Respond(Reply {
                content,
                finish_reason,
                input: count("input"),
                output: count("output"),
            })
        }
        _ => return Err("unknown action".to_owned()),
    };
    check_action(hook, &action)?;
    Ok(action)
}

/// The rules every carrier's answer follows: which actions a hook allows, what a patch may touch
/// and which headers may be set.
pub fn check_action(hook: Hook, action: &HookAction) -> Result<(), String> {
    match action {
        HookAction::Continue => Ok(()),
        HookAction::Respond(_) if hook != Hook::Request => Err("only a request hook can respond".to_owned()),
        HookAction::Respond(r)
            if !matches!(r.finish_reason.as_str(), "stop" | "length" | "content_filter") =>
        {
            Err("unknown finish_reason".to_owned())
        }
        HookAction::Respond(_) => Ok(()),
        HookAction::Deny { .. } if hook == Hook::Attempt => Err("an attempt hook cannot deny".to_owned()),
        HookAction::Deny { status, .. } if !matches!(status, 400 | 403) => {
            Err("a deny status must be 400 or 403".to_owned())
        }
        HookAction::Deny { .. } => Ok(()),
        HookAction::Mutate {
            patch, set_headers, ..
        } => {
            if matches!(hook, Hook::Request | Hook::Attempt)
                && patch.iter().flat_map(touched).any(forbidden_path)
            {
                return Err("the patch touches /model or /stream".to_owned());
            }
            if hook == Hook::StreamChunk && !set_headers.is_empty() {
                return Err("a stream_chunk answer cannot set headers".to_owned());
            }
            for (name, value) in set_headers {
                if antirion_egress::headers::always_denied(name)
                    || http_name_invalid(name)
                    || name.bytes().any(|b| b.is_ascii_uppercase())
                    || value.bytes().any(|b| b == b'\r' || b == b'\n')
                {
                    return Err(format!("header {name:?} cannot be set by an extension"));
                }
            }
            Ok(())
        }
    }
}

fn http_name_invalid(name: &str) -> bool {
    name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_read_strictly() {
        let ok = |s: &str| parse_answer(Hook::Request, 200, s.as_bytes());
        assert_eq!(ok(r#"{"action":"continue"}"#), Ok(HookAction::Continue));
        assert!(ok(r#"{"action":"explode"}"#).is_err());
        assert!(ok("not json").is_err());
        assert!(parse_answer(Hook::Request, 500, br#"{"action":"continue"}"#).is_err());
        for bad in [
            r#"{"action":"mutate","body_patch":[{"op":"replace","path":"/model","value":"x"}]}"#,
            r#"{"action":"mutate","body_patch":[{"op":"add","path":"/stream","value":true}]}"#,
            r#"{"action":"mutate","set_headers":{"x-antirion-pin-deployment":"d"}}"#,
            r#"{"action":"mutate","set_headers":{"authorization":"Bearer x"}}"#,
            r#"{"action":"deny","status":429,"message":"x"}"#,
        ] {
            assert!(ok(bad).is_err(), "{bad}");
        }
        assert_eq!(
            ok(r#"{"action":"deny","status":403,"message":"no"}"#),
            Ok(HookAction::Deny {
                status: 403,
                message: "no".into()
            })
        );
        assert!(parse_answer(Hook::Response, 200, br#"{"action":"respond","content":"x"}"#).is_err());
    }

    #[test]
    fn each_hook_allows_its_actions() {
        let deny = br#"{"action":"deny","status":403,"message":"no"}"#;
        assert!(parse_answer(Hook::StreamChunk, 200, deny).is_ok());
        assert!(parse_answer(Hook::Attempt, 200, deny).is_err());
        let model = br#"{"action":"mutate","body_patch":[{"op":"replace","path":"/model","value":"x"}]}"#;
        assert!(parse_answer(Hook::Attempt, 200, model).is_err());
        assert!(parse_answer(Hook::StreamChunk, 200, model).is_ok());
        let headers = br#"{"action":"mutate","set_headers":{"x-a":"b"}}"#;
        assert!(parse_answer(Hook::StreamChunk, 200, headers).is_err());
        assert!(parse_answer(Hook::Attempt, 200, headers).is_ok());
        assert!(parse_answer(Hook::StreamChunk, 200, br#"{"action":"respond","content":"x"}"#).is_err());
    }

    #[test]
    fn chunk_and_attempt_payloads() {
        use crate::api::{AttemptTarget, Event};
        let facts = HookFacts::default();
        let config = json!({"k": 1});
        let data = json!({"type": "content_block_delta"});
        let input = Input::new(Hook::StreamChunk, "e", &config, &facts).with_event(Event::new(
            3,
            Some("delta"),
            &data,
        ));
        let p: Value = serde_json::from_slice(&payload_of(&input, true, &[])).unwrap();
        assert_eq!(p["hook"], "stream_chunk");
        assert_eq!(p["event"], json!({"index": 3, "event": "delta", "data": data}));
        let p: Value = serde_json::from_slice(&payload_of(&input, false, &[])).unwrap();
        assert!(p["event"].get("data").is_none());
        let target = AttemptTarget::new(1, "d", "p", "openai", "gpt");
        let body = json!({"model": "gpt"});
        let input = Input::new(Hook::Attempt, "e", &config, &facts)
            .with_attempt(&target)
            .with_body(Some(&body));
        let p: Value = serde_json::from_slice(&payload_of(&input, true, &[])).unwrap();
        assert_eq!(p["attempt"]["index"], 1);
        assert_eq!(p["attempt"]["deployment"], "d");
        assert_eq!(p["body"], body);
    }

    #[test]
    fn signature_scheme() {
        let s = signature("k", b"{}", 1700000000).unwrap();
        assert!(s.starts_with("t=1700000000,v1=") && s.len() == "t=1700000000,v1=".len() + 64);
    }
}

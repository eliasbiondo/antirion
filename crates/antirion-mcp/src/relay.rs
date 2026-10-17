//! Relaying one routed request's answer to the client: each upstream message is shaped (namespacing,
//! server-to-client id rewriting, error decoration, modern decorations) and, for streamed answers,
//! sent as it arrives with the idle and total bounds enforced after the response committed.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::Stage;
use antirion_error::{Code, GatewayError, Message};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::StreamExt;
use http::Response;
use serde_json::{Map, Value};
use tokio::time::Instant;

use crate::compile::ServerRt;
use crate::http::Env;
use crate::link::{Link, next_message};
use crate::lists::Kind;
use crate::names;
use crate::route::ResultCheck;
use crate::rpc::{Fail, credentials, decorate_upstream_error};
use crate::state::{Hints, InFlight};
use crate::upstream::{Source, UpError};
use crate::{Body, respond};

/// The `ag1` form of a server-to-client request id, so any node can route the client's answer.
pub(crate) fn encode_id(server: &str, id: &Value) -> Option<String> {
    match id {
        Value::Number(n) => Some(format!("ag1:{server}:n:{n}")),
        Value::String(s) => Some(format!("ag1:{server}:s:{}", URL_SAFE_NO_PAD.encode(s.as_bytes()))),
        _ => None,
    }
}

/// `(server, original id)` of an `ag1` id.
pub(crate) fn decode_id(id: &Value) -> Option<(String, Value)> {
    let rest = id.as_str()?.strip_prefix("ag1:")?;
    let (server, rest) = rest.split_once(':')?;
    let (kind, value) = rest.split_once(':')?;
    let original = match kind {
        "n" => {
            let n: serde_json::Number = value.parse().ok()?;
            Value::Number(n)
        }
        "s" => Value::String(String::from_utf8(URL_SAFE_NO_PAD.decode(value).ok()?).ok()?),
        _ => return None,
    };
    Some((server.to_owned(), original))
}

/// Modern decorations to apply to the final result.
#[derive(Clone, Debug, Default)]
pub(crate) struct ModernShape {
    pub(crate) server_info: Option<Value>,
    /// For cacheable results: the TTL cap in ms and whether the result depends on who asks.
    pub(crate) cache: Option<(i64, bool)>,
}

/// How one routed answer is shaped for the client.
#[derive(Clone, Debug)]
pub(crate) struct Shaper {
    pub(crate) env: Env,
    pub(crate) server: Arc<ServerRt>,
    /// The server's prefix when names are namespaced.
    pub(crate) prefix: Option<String>,
    pub(crate) method: String,
    pub(crate) client_id: Value,
    pub(crate) modern: Option<ModernShape>,
}

impl Shaper {
    pub(crate) fn is_final(&self, msg: &Value) -> bool {
        msg.get("id") == Some(&self.client_id) && (msg.get("result").is_some() || msg.get("error").is_some())
    }

    pub(crate) fn shape(&self, mut msg: Value) -> Value {
        if self.is_final(&msg) {
            if msg.get("error").is_some() {
                let secrets = credentials(&self.server.headers);
                decorate_upstream_error(&mut msg, &self.server.name, &self.env.request_id, &secrets);
            } else if let Some(result) = msg.get_mut("result") {
                self.result(result);
            }
            return msg;
        }
        shape_unsolicited(&self.env, &self.server, self.prefix.as_deref(), msg)
    }

    fn result(&self, result: &mut Value) {
        if let Some(prefix) = &self.prefix {
            match self.method.as_str() {
                "tools/call" => names::tool_result(result, prefix),
                "resources/read" => names::read_result(result, prefix),
                _ => {}
            }
        }
        let Some(modern) = &self.modern else {
            return;
        };
        let hints = Hints::of(result);
        let Some(obj) = result.as_object_mut() else {
            return;
        };
        obj.entry("resultType").or_insert_with(|| Value::from("complete"));
        if let Some(info) = &modern.server_info {
            let meta = obj.entry("_meta").or_insert_with(|| Value::Object(Map::new()));
            if let Some(m) = meta.as_object_mut() {
                m.insert(crate::fanout::SERVER_INFO_META.into(), info.clone());
            }
        }
        if let Some((cap, private)) = modern.cache {
            obj.insert("ttlMs".into(), Value::from(hints.ttl_ms.min(cap).max(0)));
            let public = hints.public && !private;
            obj.insert(
                "cacheScope".into(),
                Value::from(if public { "public" } else { "private" }),
            );
        }
    }
}

/// Shapes a message the server sent on its own: server-to-client requests get `ag1` ids, URIs of
/// update notifications are namespaced, and `list_changed` invalidates the cached lists.
pub(crate) fn shape_unsolicited(env: &Env, server: &ServerRt, prefix: Option<&str>, mut msg: Value) -> Value {
    let Some(method) = msg.get("method").and_then(Value::as_str).map(str::to_owned) else {
        return msg;
    };
    if let Some(id) = msg.get("id").cloned() {
        if let (Some(new), Some(obj)) = (encode_id(&server.name, &id), msg.as_object_mut()) {
            obj.insert("id".into(), Value::from(new));
        }
        return msg;
    }
    if method == "notifications/resources/updated"
        && let Some(p) = prefix
    {
        names::updated_notification(&mut msg, p);
    }
    for kind in Kind::of_notification(&method) {
        env.state.invalidate(server.generation, kind.method());
    }
    msg
}

fn timeout_event(shaper: &Shaper) -> Value {
    let fail = Fail::from(GatewayError::new(
        Code::UpstreamTimeout,
        Stage::Attempt,
        Message::Fixed("The MCP server stopped answering."),
    ))
    .provider(&shaper.server.name)
    .id(&shaper.client_id);
    fail.render(&shaper.env.request_id).2
}

fn broken_event(shaper: &Shaper) -> Value {
    let fail = UpError::Reset.fail(&shaper.server.name).id(&shaper.client_id);
    fail.render(&shaper.env.request_id).2
}

/// Streaming state of one relayed answer.
struct Relay {
    src: Source,
    shaper: Shaper,
    deadline: Instant,
    idle: Duration,
    done: bool,
    /// A link opened for this request alone, closed when the answer ends.
    owned: Option<Link>,
    _inflight: Option<InFlight>,
    /// Result hooks the final answer goes through.
    check: Option<ResultCheck>,
}

impl Relay {
    fn wait(&self) -> Duration {
        self.idle
            .min(self.deadline.saturating_duration_since(Instant::now()))
    }

    async fn finish(&mut self) {
        if let Some(link) = self.owned.take() {
            link.close(&self.shaper.env).await;
        }
    }
}

/// The final answer through the result hooks; a refusal becomes the error answering the request.
async fn checked(check: Option<&ResultCheck>, shaper: &Shaper, msg: Value) -> Value {
    match check {
        Some(c) => match c.apply(&shaper.env, msg).await {
            Ok(m) => m,
            Err(f) => f.id(&shaper.client_id).render(&shaper.env.request_id).2,
        },
        None => msg,
    }
}

/// Answers with the streamed messages of `src`. The first message is awaited before the response
/// commits, so a silent server still gets a plain 504, and a first message that is already the
/// final answer goes through the result hooks before it, so their refusal keeps its own status.
pub(crate) async fn stream(
    mut src: Source,
    shaper: Shaper,
    owned: Option<Link>,
    inflight: Option<InFlight>,
    check: Option<ResultCheck>,
) -> Result<Response<Body>, Fail> {
    let server = Arc::clone(&shaper.server);
    let deadline = Instant::now() + server.request_timeout;
    let idle = server.stream_idle_timeout;
    let first_wait = idle.min(server.request_timeout);
    let first = match next_message(&mut src, first_wait).await {
        Ok(m) => m,
        Err(e) => {
            if let Some(link) = owned {
                link.close(&shaper.env).await;
            }
            return Err(e.fail(&server.name).id(&shaper.client_id));
        }
    };
    let done = shaper.is_final(&first);
    let mut first = shaper.shape(first);
    if done && let Some(c) = &check {
        first = match c.apply(&shaper.env, first).await {
            Ok(m) => m,
            Err(f) => {
                if let Some(link) = owned {
                    link.close(&shaper.env).await;
                }
                return Err(f.id(&shaper.client_id));
            }
        };
    }
    let relay = Relay {
        src,
        shaper,
        deadline,
        idle,
        done,
        owned,
        _inflight: inflight,
        check,
    };
    let first = respond::event(&first);
    let rest = futures_util::stream::unfold(relay, |mut r| async move {
        if r.done {
            r.finish().await;
            return None;
        }
        let wait = r.wait();
        let out = match next_message(&mut r.src, wait).await {
            Ok(msg) => {
                r.done = r.shaper.is_final(&msg);
                let shaped = r.shaper.shape(msg);
                if r.done {
                    checked(r.check.as_ref(), &r.shaper, shaped).await
                } else {
                    shaped
                }
            }
            Err(UpError::Timeout) => {
                r.done = true;
                timeout_event(&r.shaper)
            }
            Err(_) => {
                r.done = true;
                broken_event(&r.shaper)
            }
        };
        Some((respond::event(&out), r))
    });
    let events = futures_util::stream::once(async move { first }).chain(rest);
    Ok(respond::sse(events))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn ag1_ids_round_trip_with_their_type() {
        assert_eq!(encode_id("b", &json!(7)).unwrap(), "ag1:b:n:7");
        assert_eq!(encode_id("b", &json!("abc")).unwrap(), "ag1:b:s:YWJj");
        assert_eq!(encode_id("b", &json!(0)).unwrap(), "ag1:b:n:0");
        assert_eq!(decode_id(&json!("ag1:b:n:7")), Some(("b".into(), json!(7))));
        assert_eq!(
            decode_id(&json!("ag1:b:s:YWJj")),
            Some(("b".into(), json!("abc")))
        );
        assert_eq!(decode_id(&json!("ag1:b:n:notanumber")), None);
        assert_eq!(decode_id(&json!("ag1:b:s:!!")), None);
        assert_eq!(decode_id(&json!("plain-id")), None);
        assert_eq!(decode_id(&json!(17)), None);
    }
}

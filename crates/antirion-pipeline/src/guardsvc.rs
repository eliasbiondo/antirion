//! The calls of judge, classifier and partner guardrails: a judge asks its Model through the first
//! usable deployment that speaks OpenAI Chat (with the verdict's response format) or Anthropic
//! Messages; classifiers and partners are JSON POSTs through the egress connector, authorised
//! with a provider's credentials when the guardrail names one.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::{RequestId, headers as h};
use antirion_egress::{OutboundRequest, TrustClass};
use antirion_guardrails::services::{Judge, ServiceAuth, ServiceCall};
use antirion_providers::{Native, UpstreamCall, UpstreamOp};
use antirion_runtime::{Runtime, Snapshot};
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{HeaderName, HeaderValue, Method, Uri};
use serde_json::Value;

use crate::external::header_map;
use crate::outcome::collect;

/// The largest answer read.
const MAX_ANSWER: u64 = 1 << 20;
/// Callers bound each call with their own timeout; the transport's bounds only back it up.
const SLACK: Duration = Duration::from_secs(1);

fn answer_text(v: &Value, anthropic: bool) -> Option<String> {
    if anthropic {
        let text: String = v
            .get("content")?
            .as_array()?
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect();
        return Some(text);
    }
    v.pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Asks the judge's Model `message`; the text of the first answer.
pub(crate) async fn judge(
    rt: &Runtime,
    snap: &Snapshot,
    request_id: &RequestId,
    judge: &Judge,
    message: &str,
    timeout: Duration,
) -> Result<String, String> {
    let resolved = snap
        .routing
        .resolve(&judge.model)
        .ok_or_else(|| format!("model {} does not exist", judge.model))?;
    let candidates = crate::authorize::candidates(&snap.providers, &snap.routing, &resolved.model);
    let mut last =
        "the judge model has no deployment that speaks OpenAI Chat or Anthropic Messages".to_owned();
    for d in candidates {
        if !d.usable() {
            continue;
        }
        let anthropic = if d.speaks(Native::OpenAi) {
            false
        } else if d.speaks(Native::AnthropicMessages) {
            true
        } else {
            continue;
        };
        let (op, body) = if anthropic {
            (UpstreamOp::Messages, judge.messages_body(&d.model, message))
        } else {
            (
                UpstreamOp::OpenAi {
                    class: "generate",
                    path: "/chat/completions",
                },
                judge.chat_body(&d.model, message),
            )
        };
        let prepared = d.build_request(UpstreamCall {
            op,
            stream: false,
            body: Bytes::from(serde_json::to_vec(&body).unwrap_or_default()).into(),
            content_type: Some(HeaderValue::from_static("application/json")),
            request_id,
            response_timeout: timeout,
            connect_timeout: Some(timeout),
            anthropic: None,
            key: None,
            byok: None,
        });
        let Ok(prepared) = prepared else {
            last = "the judge request could not be built".to_owned();
            continue;
        };
        let mut request = prepared.request;
        request.resend_when_closed = false;
        let tls = snap
            .security
            .egress_tls
            .clone()
            .unwrap_or_else(|| d.provider.tls.clone());
        if d.authorize(&mut request, &rt.creds, &tls).await.is_err() {
            last = "no credential for the judge deployment".to_owned();
            continue;
        }
        let up = match rt.egress.send(request).await {
            Ok(up) => up,
            Err(e) => {
                last = e.to_string();
                continue;
            }
        };
        let status = up.status.as_u16();
        let raw = collect(up.body, MAX_ANSWER)
            .await
            .map_err(|_| "the judge's answer could not be read".to_owned())?;
        if !(200..300).contains(&status) {
            return Err(format!("the judge model answered with status {status}"));
        }
        let v: Value =
            serde_json::from_slice(&raw).map_err(|_| "the judge's answer is not JSON".to_owned())?;
        return answer_text(&v, anthropic).ok_or_else(|| "the judge's answer has no text".to_owned());
    }
    Err(last)
}

/// POSTs a service call; the answer's status and body.
pub(crate) async fn post(
    rt: &Runtime,
    snap: &Snapshot,
    request_id: &RequestId,
    call: ServiceCall,
    timeout: Duration,
) -> Result<(u16, Vec<u8>), String> {
    let uri: Uri = call.url.parse().map_err(|_| "the URL is not valid".to_owned())?;
    let tls = snap
        .security
        .egress_tls
        .clone()
        .ok_or_else(|| "no egress TLS profile".to_owned())?;
    let mut headers = header_map(&call.headers);
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Ok(v) = HeaderValue::from_str(request_id.as_str()) {
        headers.insert(HeaderName::from_static(h::X_REQUEST_ID), v);
    }
    let mut request = OutboundRequest {
        method: Method::POST,
        uri,
        headers,
        body: Bytes::from(call.body).into(),
        response_timeout: timeout + SLACK,
        connect_timeout: Some(timeout + SLACK),
        fresh_connection: false,
        trust: TrustClass::Operator,
        tls: tls.clone(),
        policy: Arc::clone(&snap.security.egress),
        resend_when_closed: false,
    };
    if let ServiceAuth::Provider { name, region } = &call.auth {
        let provider = snap
            .providers
            .providers
            .get(name)
            .ok_or_else(|| format!("provider {name} does not exist"))?;
        provider
            .authorize_service(&mut request, &rt.creds, &tls, region.as_deref())
            .await
            .map_err(|_| format!("no credential from provider {name}"))?;
    }
    let sent = rt.egress.send(request).await.map_err(|e| e.to_string())?;
    let status = sent.status.as_u16();
    collect(sent.body, MAX_ANSWER)
        .await
        .map(|b| (status, b.to_vec()))
        .map_err(|_| "the answer could not be read".to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn answers_are_read_in_both_dialects() {
        let openai = json!({"choices": [{"message": {"content": "{\"passed\":true}"}}]});
        assert_eq!(answer_text(&openai, false).as_deref(), Some("{\"passed\":true}"));
        let anthropic = json!({"content": [{"type": "text", "text": "{\"passed\":"}, {"type": "text", "text": "false}"}]});
        assert_eq!(
            answer_text(&anthropic, true).as_deref(),
            Some("{\"passed\":false}")
        );
    }
}

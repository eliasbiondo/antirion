//! Calls the gateway makes on a request's behalf to operator-configured services: guardrail and
//! extension webhooks through the egress connector (operator trust, SSRF checks, never re-sent),
//! and moderation calls through the moderation Model's deployment.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::{RequestId, headers as h};
use antirion_egress::{OutboundRequest, TrustClass};
use antirion_guardrails::def::Webhook;
use antirion_providers::{UpstreamCall, UpstreamOp};
use antirion_runtime::{Runtime, Snapshot};
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Uri};
use serde_json::json;

use crate::internal::{Answer, Question, ask_for_guardrail};
use crate::outcome::{Unread, collect};

/// The largest guardrail webhook or moderation answer read.
const MAX_ANSWER: u64 = 1 << 20;
/// Callers bound each call with their own timeout; the transport's bounds only back it up.
const SLACK: Duration = Duration::from_secs(1);

/// Why an external call produced no answer.
#[derive(Debug)]
pub(crate) enum CallError {
    /// The configured answer size was exceeded.
    TooLarge,
    Failed(String),
}

impl CallError {
    pub(crate) fn text(&self) -> String {
        match self {
            CallError::TooLarge => "the answer is larger than allowed".to_owned(),
            CallError::Failed(s) => s.clone(),
        }
    }
}

/// One JSON POST to an operator URL.
#[derive(Debug)]
pub(crate) struct Post<'a> {
    pub(crate) url: &'a str,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Vec<u8>,
    pub(crate) timeout: Duration,
    /// The largest answer read.
    pub(crate) max: u64,
}

/// POSTs a JSON body to an operator URL; the answer's status and body.
pub(crate) async fn post_json(
    rt: &Runtime,
    snap: &Snapshot,
    request_id: &RequestId,
    post: Post<'_>,
) -> Result<(u16, Vec<u8>), CallError> {
    let Post {
        url,
        mut headers,
        body,
        timeout,
        max,
    } = post;
    let uri: Uri = url
        .parse()
        .map_err(|_| CallError::Failed("the URL is not valid".to_owned()))?;
    let tls = snap
        .security
        .egress_tls
        .clone()
        .ok_or_else(|| CallError::Failed("no egress TLS profile".to_owned()))?;
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Ok(v) = HeaderValue::from_str(request_id.as_str()) {
        headers.insert(HeaderName::from_static(h::X_REQUEST_ID), v);
    }
    let sent = rt
        .egress
        .send(OutboundRequest {
            method: Method::POST,
            uri,
            headers,
            body: Bytes::from(body).into(),
            response_timeout: timeout + SLACK,
            connect_timeout: Some(timeout + SLACK),
            fresh_connection: false,
            trust: TrustClass::Operator,
            tls,
            policy: Arc::clone(&snap.security.egress),
            resend_when_closed: false,
        })
        .await
        .map_err(|e| CallError::Failed(e.to_string()))?;
    let status = sent.status.as_u16();
    match collect(sent.body, max).await {
        Ok(b) => Ok((status, b.to_vec())),
        Err(Unread::TooLarge) => Err(CallError::TooLarge),
        Err(_) => Err(CallError::Failed("the answer was cut short".to_owned())),
    }
}

/// Configured header pairs as a header map; pairs that are not valid headers are dropped.
pub(crate) fn header_map(pairs: &[(String, String)]) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (k, v) in pairs {
        if let (Ok(n), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            out.insert(n, v);
        }
    }
    out
}

/// The network of guardrail checks for one request.
#[derive(Clone, Debug)]
pub(crate) struct GuardNet {
    pub(crate) rt: Arc<Runtime>,
    pub(crate) snap: Arc<Snapshot>,
    pub(crate) request_id: RequestId,
}

impl antirion_guardrails::Net for GuardNet {
    async fn webhook(
        &self,
        hook: &Webhook,
        body: Vec<u8>,
        timeout: Duration,
    ) -> Result<(u16, Vec<u8>), String> {
        let mut headers = header_map(&hook.headers);
        if let Some(sig) = hook
            .signing_secret
            .as_ref()
            .map(antirion_core::Secret::expose)
            .and_then(|s| antirion_guardrails::webhook::signature(s, &body))
            .and_then(|s| HeaderValue::from_str(&s).ok())
        {
            headers.insert(HeaderName::from_static("x-antirion-signature"), sig);
        }
        let post = Post {
            url: &hook.url,
            headers,
            body,
            timeout,
            max: MAX_ANSWER,
        };
        post_json(&self.rt, &self.snap, &self.request_id, post)
            .await
            .map_err(|e| e.text())
    }

    async fn moderate(
        &self,
        model: &str,
        inputs: Vec<String>,
        timeout: Duration,
    ) -> Result<(u16, Vec<u8>), String> {
        let resolved = self
            .snap
            .routing
            .resolve(model)
            .ok_or_else(|| format!("model {model} does not exist"))?;
        let candidates =
            crate::authorize::candidates(&self.snap.providers, &self.snap.routing, &resolved.model);
        let mut last = "the moderation model has no deployment".to_owned();
        for d in candidates {
            let body = json!({"model": d.model, "input": inputs});
            let prepared = d.build_request(UpstreamCall {
                op: UpstreamOp::OpenAi {
                    class: "moderate",
                    path: "/moderations",
                },
                stream: false,
                body: Bytes::from(serde_json::to_vec(&body).unwrap_or_default()).into(),
                content_type: Some(HeaderValue::from_static("application/json")),
                request_id: &self.request_id,
                response_timeout: timeout,
                connect_timeout: Some(timeout),
                anthropic: None,
                key: None,
                byok: None,
            });
            let Ok(prepared) = prepared else {
                last = "the moderation request could not be built".to_owned();
                continue;
            };
            let mut request = prepared.request;
            request.resend_when_closed = false;
            let tls = self
                .snap
                .security
                .egress_tls
                .clone()
                .unwrap_or_else(|| d.provider.tls.clone());
            if d.authorize(&mut request, &self.rt.creds, &tls).await.is_err() {
                last = "no credential for the moderation deployment".to_owned();
                continue;
            }
            match self.rt.egress.send(request).await {
                Ok(up) => {
                    let status = up.status.as_u16();
                    return match collect(up.body, MAX_ANSWER).await {
                        Ok(b) => Ok((status, b.to_vec())),
                        Err(_) => Err("the moderation answer could not be read".to_owned()),
                    };
                }
                Err(e) => last = e.to_string(),
            }
        }
        Err(last)
    }

    async fn judge(
        &self,
        judge: &antirion_guardrails::services::Judge,
        message: String,
        timeout: Duration,
    ) -> Result<String, String> {
        crate::guardsvc::judge(&self.rt, &self.snap, &self.request_id, judge, &message, timeout).await
    }

    async fn post(
        &self,
        call: antirion_guardrails::services::ServiceCall,
        timeout: Duration,
    ) -> Result<(u16, Vec<u8>), String> {
        crate::guardsvc::post(&self.rt, &self.snap, &self.request_id, call, timeout).await
    }

    async fn chat(
        &self,
        model: &str,
        system: &str,
        user: &str,
        max_tokens: u32,
        timeout: Duration,
    ) -> Result<String, String> {
        let q = Question::Chat {
            system,
            user,
            max_tokens,
        };
        match ask_for_guardrail(&self.rt, &self.snap, &self.request_id, model, q, timeout).await? {
            Answer::Text(t) => Ok(t),
            Answer::Vectors(_) => Err("the answer is malformed".to_owned()),
        }
    }

    async fn embed(
        &self,
        model: &str,
        inputs: Vec<String>,
        timeout: Duration,
    ) -> Result<Vec<Vec<f32>>, String> {
        let q = Question::Embed(&inputs);
        match ask_for_guardrail(&self.rt, &self.snap, &self.request_id, model, q, timeout).await? {
            Answer::Vectors(v) => Ok(v),
            Answer::Text(_) => Err("the answer is malformed".to_owned()),
        }
    }
}

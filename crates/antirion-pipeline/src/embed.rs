//! Embeddings and rerank in a translated cell: the client body encoded for the provider's API, one
//! or more upstream calls in one attempt (Titan embeds one text per call), and the answers decoded
//! into the client's shape, which is then delivered like any complete response.

use std::time::Duration;

use antirion_dialects::embed::{self as codec, EmbedApi};
use antirion_dialects::rerank::{self as ranker, RerankApi};
use antirion_dialects::xlate::warn::{Kind as WarnKind, Src, Warnings};
use antirion_egress::Payload;
use antirion_error::reason::hygiene;
use antirion_error::{Code, Message};
use antirion_obs::trace::SpanId;
use antirion_providers::{DeploymentRt, UpstreamCall, UpstreamOp};
use antirion_resilience::{Attempt, Class, Failure, Sent, TimerKind};
use bytes::Bytes;
use http::{HeaderMap, HeaderValue};
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::attempt::{Done, Plan};
use crate::cell::Cell;
use crate::ctx::Ctx;
use crate::outcome::{self, json::Answer};
use crate::parse::Parsed;
use crate::route::Position;

/// The upstream operation of an embedding API.
pub(crate) fn upstream_op(api: EmbedApi, batch: bool) -> UpstreamOp<'static> {
    match api {
        EmbedApi::Gemini => UpstreamOp::GeminiEmbed { batch },
        EmbedApi::VertexPredict => UpstreamOp::VertexPredict,
        EmbedApi::Titan | EmbedApi::BedrockCohere => UpstreamOp::Invoke,
        EmbedApi::Cohere => UpstreamOp::CohereEmbed,
    }
}

/// The upstream operation of a rerank API.
pub(crate) fn rerank_op(api: RerankApi) -> UpstreamOp<'static> {
    match api {
        RerankApi::Cohere => UpstreamOp::CohereRerank,
        RerankApi::Bedrock => UpstreamOp::BedrockRerank,
        RerankApi::Compatible | RerankApi::TopK => UpstreamOp::OpenAi {
            class: "rerank",
            path: "/rerank",
        },
    }
}

/// Whether an embeddings body holds only text inputs, which other providers can embed.
pub(crate) fn text_inputs(parsed: &Parsed) -> bool {
    parsed.doc().is_some_and(|d| codec::decode_request(d).is_ok())
}

/// The members an embeddings request maps for another provider.
const EMBED_MEMBERS: &[&str] = &[
    "model",
    "input",
    "dimensions",
    "encoding_format",
    "input_type",
    "task_type",
    "title",
];

/// The members a rerank request maps.
const RERANK_MEMBERS: &[&str] = &[
    "model",
    "query",
    "documents",
    "top_n",
    "return_documents",
    "max_tokens_per_doc",
];

/// Entries for the client members a translation does not carry.
fn ignored(doc: &Value, mapped: &[&str]) -> Warnings {
    let mut w = Warnings::default();
    if let Some(obj) = doc.as_object() {
        for name in obj.keys().filter(|k| !mapped.contains(&k.as_str())) {
            w.add(WarnKind::Ignored, &Src::root().key(obj, name));
        }
    }
    w
}

/// What the calls of one attempt brought back.
struct Answers {
    bodies: Vec<Value>,
    status: u16,
    headers: HeaderMap,
}

/// Sends one attempt of an embeddings or rerank request in a translated cell.
pub(crate) async fn send(
    cx: &Ctx,
    plan: &Plan<'_>,
    a: &Attempt<'_>,
    position: &Position,
    parsed: &Parsed,
    cell: Cell,
) -> Sent<Done> {
    let d = &position.deployment;
    let attempts = u16::try_from(a.index + 1).unwrap_or(u16::MAX);
    let att = outcome::Attempt {
        cx,
        plan,
        deployment: d,
        cell,
        attempts,
        started: Instant::now(),
        started_at: cx.rt.now(),
        key: a.key.map(str::to_owned),
        wait: a.wait,
        via: a.via.map(str::to_owned),
        first: a.first.to_owned(),
        timers: a.timers.clone(),
        inflight: Some(std::sync::Arc::new(cx.rt.state.routing.begin(&d.name))),
        acquired: Vec::new(),
        span: SpanId::random(),
        server: None,
        xlate: None,
        refusal_fails: false,
        byok: None,
        aggregate: false,
        predicates: &[],
        race: None,
    };
    let Some(doc) = parsed.doc() else {
        let f = att.fail(
            Class::CallerError,
            Code::InvalidRequest,
            None,
            None,
            Message::Fixed("The request body cannot be translated."),
        );
        att.record(None, Class::CallerError, None);
        return Sent::Failed(f);
    };
    let result = match cell {
        Cell::Embed(api) => embed(&att, a, doc, api).await,
        Cell::Rerank(api) => rerank(&att, a, doc, api).await,
        _ => Err(att.fail(
            Class::CallerError,
            Code::Internal,
            None,
            None,
            Message::Fixed("The cell is not an embeddings or rerank cell."),
        )),
    };
    match result {
        Ok(done) => Sent::Success(done),
        Err(f) => {
            att.record_failure(f.status, &f);
            Sent::Failed(f)
        }
    }
}

async fn embed(
    att: &outcome::Attempt<'_>,
    a: &Attempt<'_>,
    doc: &Value,
    api: EmbedApi,
) -> Result<Done, Failure> {
    let d = att.deployment;
    let req = codec::decode_request(doc).map_err(|u| {
        att.fail(
            Class::CallerError,
            Code::InvalidRequest,
            Some(crate::reasons::UNTRANSLATABLE_REQUEST),
            None,
            Message::built(format!(
                "{} cannot be sent to provider `{}`.",
                u.param, d.provider.name
            )),
        )
    })?;
    let bodies = codec::encode(api, &req, &d.model);
    let answers = calls(att, a, upstream_op(api, !req.single), bodies).await?;
    let Some(embedded) = codec::decode_answers(api, &answers.bodies, req.texts.len()) else {
        return Err(att.bad(
            hygiene::INVALID_BODY,
            answers.status,
            "upstream response missing the embeddings".to_owned(),
        ));
    };
    let shown = outcome::response_model(att, &d.model);
    let client = codec::client_body(&embedded, &shown, req.base64);
    let mut warnings = ignored(doc, EMBED_MEMBERS);
    if embedded.tokens.is_none() {
        warnings.response(WarnKind::Ignored, "usage");
    }
    let facts = facts(&shown, None, embedded.tokens);
    deliver(att, answers, client, facts, warnings).await
}

async fn rerank(
    att: &outcome::Attempt<'_>,
    a: &Attempt<'_>,
    doc: &Value,
    api: RerankApi,
) -> Result<Done, Failure> {
    let d = att.deployment;
    let req = ranker::decode_request(doc);
    let body = ranker::encode(api, &req, &d.model, d.region.as_deref().unwrap_or("us-east-1"));
    let answers = calls(att, a, rerank_op(api), vec![body]).await?;
    let ranked = answers
        .bodies
        .first()
        .and_then(|v| ranker::decode_answer(api, v, req.documents.len()));
    let Some(ranked) = ranked else {
        return Err(att.bad(
            hygiene::INVALID_BODY,
            answers.status,
            "upstream response missing the rerank results".to_owned(),
        ));
    };
    let tokens = ranked.tokens;
    let id = outcome::upstream_or_new_id(&answers.headers);
    let client = ranker::client_body(&req, ranked, &id);
    let shown = outcome::response_model(att, &d.model);
    let facts = facts(&shown, client.get("id").and_then(Value::as_str), tokens);
    deliver(att, answers, client, facts, ignored(doc, RERANK_MEMBERS)).await
}

/// The body settlement and the record read: the model, the id, and the input tokens the provider
/// reported (without them, settlement estimates the input).
fn facts(model: &str, id: Option<&str>, tokens: Option<u64>) -> Value {
    let usage = match tokens {
        Some(t) => json!({"prompt_tokens": t, "completion_tokens": 0, "total_tokens": t}),
        None => json!({"completion_tokens": 0}),
    };
    json!({"model": model, "id": id, "usage": usage})
}

async fn deliver(
    att: &outcome::Attempt<'_>,
    answers: Answers,
    client: Value,
    facts: Value,
    warnings: Warnings,
) -> Result<Done, Failure> {
    let raw = Bytes::from(serde_json::to_vec(&client).unwrap_or_default());
    let shown = facts
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    outcome::json::deliver(
        att,
        Answer {
            status: answers.status,
            raw,
            facts,
            shown,
            translated: Ok(Some(client)),
            warnings: Some(warnings),
            content_type: Some(HeaderValue::from_static("application/json")),
            up_headers: answers.headers,
        },
    )
    .await
    .map(|(done, _)| done)
}

/// Sends each body in order on the attempt's deployment, under the attempt's timer; the first
/// call that fails fails the attempt.
async fn calls(
    att: &outcome::Attempt<'_>,
    a: &Attempt<'_>,
    op: UpstreamOp<'_>,
    bodies: Vec<Value>,
) -> Result<Answers, Failure> {
    let cx = att.cx;
    let d: &DeploymentRt = att.deployment;
    let timers = &a.timers;
    let far = att.started + Duration::from_secs(86_400 * 365);
    let (fire_at, fire_kind) = timers
        .fire_at(timers.timeouts.attempt, att.started, TimerKind::Attempt)
        .unwrap_or((far, TimerKind::Attempt));
    let connect = match timers.deadline_at {
        Some(at) => timers.connect.min(at.saturating_duration_since(att.started)),
        None => timers.connect,
    };
    let byok = att
        .plan
        .client_keys
        .as_ref()
        .and_then(|k| k.for_deployment(d))
        .filter(|_| d.byok.accepts());
    let mut out = Answers {
        bodies: Vec::with_capacity(bodies.len()),
        status: 200,
        headers: HeaderMap::new(),
    };
    for body in bodies {
        let prepared = d.build_request(UpstreamCall {
            op,
            stream: false,
            body: Payload::from(serde_json::to_vec(&body).unwrap_or_default()),
            content_type: Some(HeaderValue::from_static("application/json")),
            request_id: &cx.request_id,
            response_timeout: Duration::from_secs(86_400 * 365),
            connect_timeout: Some(connect.max(Duration::from_millis(1))),
            anthropic: None,
            key: a.key,
            byok: byok.as_deref(),
        });
        let Ok(prepared) = prepared else {
            return Err(crate::failure::of_class(
                d,
                att.attempts,
                Class::ConnectError,
                None,
                Message::Fixed("The upstream request could not be built."),
            ));
        };
        let mut request = prepared.request;
        crate::egress::outbound(
            cx,
            att.plan.client_headers,
            &att.plan.rules,
            false,
            &mut request.headers,
        );
        for (n, v) in &att.plan.extra_headers {
            request.headers.insert(n.clone(), v.clone());
        }
        cx.trace.propagate(&mut request.headers, &att.span);
        let cred_tls = cx
            .snap
            .security
            .egress_tls
            .clone()
            .unwrap_or_else(|| d.provider.tls.clone());
        let authorized = tokio::select! {
            () = cx.gone.cancelled() => return Err(crate::failure::cancelled(d, att.attempts)),
            () = tokio::time::sleep_until(fire_at) => {
                return Err(crate::failure::timeout(d, att.attempts, fire_kind, timers));
            }
            r = d.authorize(&mut request, &cx.rt.creds, &cred_tls) => r,
        };
        if authorized.is_err() {
            return Err(crate::failure::fail(
                d,
                att.attempts,
                Class::CredentialRejected,
                Code::UpstreamAuthFailed,
                Some(crate::reasons::CREDENTIAL_ACQUISITION_FAILED),
                None,
                Message::built(format!(
                    "The gateway could not obtain a credential for provider `{}`.",
                    d.provider.name
                )),
            ));
        }
        let sent = tokio::select! {
            () = cx.gone.cancelled() => return Err(crate::failure::cancelled(d, att.attempts)),
            () = tokio::time::sleep_until(fire_at) => {
                return Err(crate::failure::timeout(d, att.attempts, fire_kind, timers));
            }
            r = cx.rt.egress.send(request) => r,
        };
        let upstream = sent.map_err(|t| crate::failure::transport(d, att.attempts, &t, timers))?;
        let status = upstream.status.as_u16();
        let class = antirion_resilience::classify::status(status, true);
        if class != Class::Success {
            return Err(outcome::error_response(att, upstream, class, true).await);
        }
        let headers = upstream.headers.clone();
        let raw = match att.read(upstream.body, att.body_cap()).await? {
            Ok(b) => b,
            Err(why) => return Err(att.unread(why, status)),
        };
        let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
            return Err(att.bad(
                hygiene::INVALID_BODY,
                status,
                "upstream response is not valid json".to_owned(),
            ));
        };
        if let Some(err) = v.get("error").filter(|e| e.is_object()) {
            return Err(att.error_object(err));
        }
        if let Some(n) = headers
            .get("x-amzn-bedrock-input-token-count")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            && v.pointer("/meta/billed_units/input_tokens").is_none()
            && v.get("inputTextTokenCount").is_none()
        {
            let mut v = v;
            if let Some(o) = v.as_object_mut() {
                o.insert("meta".into(), json!({"billed_units": {"input_tokens": n}}));
            }
            out.bodies.push(v);
        } else {
            out.bodies.push(v);
        }
        out.status = status;
        out.headers = headers;
    }
    Ok(out)
}

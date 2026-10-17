//! `n > 1` for deployments that cannot honour it: the attempt sends n copies of the request
//! without `n` at once, and the answers become one Chat completion with n choices and their
//! usage summed.

use antirion_dialects::xlate::warn::{Kind, Src, Warnings};
use antirion_egress::OutboundRequest;
use antirion_error::reason::hygiene;
use antirion_providers::DeploymentRt;
use antirion_resilience::{Class, Sent, TimerKind};
use bytes::Bytes;
use serde_json::{Map, Value};
use tokio::time::Instant;

use crate::attempt::{Done, Plan};
use crate::cell::Cell;
use crate::outcome::{self, Attempt};
use crate::parse::Parsed;

/// The `n` of a request the effective `translation` policy lets fan out: a non-streaming Chat
/// request with `n` from 2 to the policy's `max`, unless the policy refuses lossy translation.
pub(crate) fn requested(translation: &Value, parsed: &Parsed, stream: bool, strict: bool) -> Option<u32> {
    let cfg = translation.get("n_fan_out")?;
    if stream || strict || cfg.get("enabled") != Some(&Value::Bool(true)) {
        return None;
    }
    let max = cfg.get("max").and_then(Value::as_u64).unwrap_or(8);
    let n = parsed.doc()?.get("n")?.as_u64()?;
    (2..=max).contains(&n).then(|| u32::try_from(n).ok()).flatten()
}

/// The `n` an attempt on `d` in `cell` fans out to, when the deployment cannot serve it itself.
pub(crate) fn calls(plan: &Plan<'_>, d: &DeploymentRt, cell: Cell) -> Option<u32> {
    let n = plan.fan_out?;
    match cell {
        Cell::Translate { .. } => Some(n),
        Cell::Identity => {
            let max = d.capabilities.get("max_n").0.as_u64().unwrap_or(u64::MAX);
            let native = d.capabilities.flag("n_multiple") && max >= u64::from(n);
            (!native).then_some(n)
        }
        _ => None,
    }
}

/// The request body without `n`.
pub(crate) fn without_n(parsed: &Parsed) -> Option<Parsed> {
    let mut v = parsed.doc()?.clone();
    v.as_object_mut()?.remove("n");
    let raw = Bytes::from(serde_json::to_vec(&v).ok()?);
    let skim = antirion_json::skim(&raw, &antirion_json::JsonLimits::default()).ok()?;
    Some(Parsed::json(raw, skim, Some(v)))
}

/// A copy of a built and authorized request; fanned-out bodies are always held whole.
fn copy(r: &OutboundRequest) -> OutboundRequest {
    OutboundRequest {
        method: r.method.clone(),
        uri: r.uri.clone(),
        headers: r.headers.clone(),
        body: r.body.clone(),
        response_timeout: r.response_timeout,
        connect_timeout: r.connect_timeout,
        fresh_connection: r.fresh_connection,
        trust: r.trust,
        tls: r.tls.clone(),
        policy: std::sync::Arc::clone(&r.policy),
        resend_when_closed: r.resend_when_closed,
    }
}

/// Adds the numbers of `from` into `into`, member by member, nested objects included.
fn add_usage(into: &mut Map<String, Value>, from: &Map<String, Value>) {
    for (k, v) in from {
        match (into.get_mut(k), v) {
            (Some(Value::Object(a)), Value::Object(b)) => add_usage(a, b),
            (Some(Value::Number(a)), Value::Number(b)) => {
                let sum = match (a.as_u64(), b.as_u64()) {
                    (Some(x), Some(y)) => Value::from(x.saturating_add(y)),
                    _ => serde_json::Number::from_f64(a.as_f64().unwrap_or(0.0) + b.as_f64().unwrap_or(0.0))
                        .map_or(Value::Null, Value::Number),
                };
                into.insert(k.clone(), sum);
            }
            (None, _) => {
                into.insert(k.clone(), v.clone());
            }
            _ => {}
        }
    }
}

/// One Chat completion of the calls' answers: their choices in call order, renumbered, and their
/// usage summed.
fn merge(answers: Vec<Value>) -> Value {
    let mut it = answers.into_iter();
    let Some(Value::Object(mut first)) = it.next() else {
        return Value::Null;
    };
    let mut choices: Vec<Value> = first
        .remove("choices")
        .and_then(|c| match c {
            Value::Array(a) => Some(a),
            _ => None,
        })
        .unwrap_or_default();
    let mut usage = match first.remove("usage") {
        Some(Value::Object(u)) => Some(u),
        _ => None,
    };
    for v in it {
        if let Some(cs) = v.get("choices").and_then(Value::as_array) {
            choices.extend(cs.iter().cloned());
        }
        if let Some(Value::Object(u)) = v.get("usage") {
            match &mut usage {
                Some(total) => add_usage(total, u),
                None => usage = Some(u.clone()),
            }
        }
    }
    for (i, c) in choices.iter_mut().enumerate() {
        if let Some(o) = c.as_object_mut() {
            o.insert("index".into(), Value::from(i));
        }
    }
    first.insert("choices".into(), Value::from(choices));
    if let Some(u) = usage {
        first.insert("usage".into(), Value::Object(u));
    }
    Value::Object(first)
}

/// The usage the finished calls reported, for an attempt that fails.
fn charge_finished(att: &Attempt<'_>, answers: &[Value]) {
    let mut total: Option<Map<String, Value>> = None;
    for v in answers {
        if let Some(Value::Object(u)) = v.get("usage") {
            match &mut total {
                Some(t) => add_usage(t, u),
                None => total = Some(u.clone()),
            }
        }
    }
    let Some(total) = total else {
        return;
    };
    let raw = Value::Object(total);
    let usage = crate::apis::usage_of(&serde_json::json!({"usage": raw}));
    if !usage.is_empty() {
        att.cx.log.update(|r| r.pending_error_usage = Some((usage, raw)));
    }
}

/// Sends `first` and n − 1 copies of it at once and answers the attempt with their merged result,
/// or with the first failure in call order.
pub(crate) async fn exchange(
    mut att: Attempt<'_>,
    first: OutboundRequest,
    n: u32,
    fire_at: Instant,
    fire_kind: TimerKind,
) -> Sent<Done> {
    let cx = att.cx;
    let d = att.deployment;
    let mut requests: Vec<OutboundRequest> = (1..n).map(|_| copy(&first)).collect();
    requests.insert(0, first);
    let sends = requests.into_iter().map(|r| cx.rt.egress.send(r));
    let all = tokio::select! {
        () = cx.gone.cancelled() => {
            att.record(None, Class::ClientCancelled, None);
            return Sent::Failed(crate::failure::cancelled(d, att.attempts));
        }
        () = tokio::time::sleep_until(fire_at) => {
            att.record(None, Class::Timeout, Some(fire_kind));
            return Sent::Failed(crate::failure::timeout(d, att.attempts, fire_kind, &att.timers));
        }
        all = futures_util::future::join_all(sends) => all,
    };
    let mut answers = Vec::with_capacity(all.len());
    for r in all {
        match r {
            Ok(up) => answers.push(up),
            Err(t) => {
                let f = crate::failure::transport(d, att.attempts, &t, &att.timers);
                att.record_failure(None, &f);
                return Sent::Failed(f);
            }
        }
    }
    let api = crate::apis::upstream_api(att.plan.endpoint, att.cell);
    let mut chats = Vec::with_capacity(answers.len());
    let mut warnings = Warnings::default();
    let mut head = None;
    for up in answers {
        let status = up.status.as_u16();
        if cx.snap.health.classify_status(status, true) != Class::Success {
            charge_finished(&att, &chats);
            return finish(&att, outcome::handle(&att, up).await);
        }
        let headers = up.headers.clone();
        let raw = match att.read(up.body, att.body_cap()).await {
            Ok(Ok(b)) => b,
            Ok(Err(why)) => return failed(&att, att.unread(why, status)),
            Err(f) => return failed(&att, f),
        };
        let Ok(v) = serde_json::from_slice::<Value>(&raw) else {
            return failed(
                &att,
                att.bad(
                    hygiene::INVALID_BODY,
                    status,
                    "upstream response is not valid json".to_owned(),
                ),
            );
        };
        if let Some(err) = v.get("error").filter(|e| e.is_object()) {
            return failed(&att, att.error_object(err));
        }
        let upstream_model = v
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&d.model)
            .to_owned();
        let shown = outcome::response_model(&att, &upstream_model);
        let client = match crate::xlate::response(&att, api, &v, &shown, &headers, status) {
            Ok((Ok(Some(c)), w)) => {
                if let Some(w) = w {
                    warnings.extend(&w);
                }
                c
            }
            Ok((Ok(None), _)) => v,
            Ok((Err(()), _)) => {
                return failed(
                    &att,
                    att.bad(
                        hygiene::INVALID_BODY,
                        status,
                        "upstream response missing 'choices'".to_owned(),
                    ),
                );
            }
            Err(f) => return failed(&att, f),
        };
        head.get_or_insert((headers, status));
        chats.push(client);
    }
    let merged = merge(chats);
    if let Some(body) = att.plan.parsed.doc().and_then(Value::as_object) {
        warnings.add(Kind::Emulated, &Src::root().key(body, "n"));
    }
    let (up_headers, status) = head.unwrap_or_default();
    att.cell = Cell::Identity;
    att.xlate = None;
    let raw = Bytes::from(serde_json::to_vec(&merged).unwrap_or_default());
    let answer = outcome::json::Answer {
        status,
        raw,
        facts: merged,
        shown: att.plan.requested.clone(),
        translated: Ok(None),
        warnings: Some(warnings),
        content_type: Some(http::HeaderValue::from_static("application/json")),
        up_headers,
    };
    finish(&att, outcome::json::deliver(&att, answer).await)
}

fn failed(att: &Attempt<'_>, f: antirion_resilience::Failure) -> Sent<Done> {
    att.record_failure(f.status, &f);
    Sent::Failed(f)
}

fn finish(att: &Attempt<'_>, r: Result<(Done, Class), antirion_resilience::Failure>) -> Sent<Done> {
    match r {
        Ok((done, _)) => Sent::Success(done),
        Err(f) => failed(att, f),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn choices_are_renumbered_and_usage_summed() {
        let a = json!({"id": "a", "object": "chat.completion", "model": "m",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "one"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6,
                      "completion_tokens_details": {"reasoning_tokens": 2}}});
        let b = json!({"id": "b", "object": "chat.completion", "model": "m",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "two"}, "finish_reason": "length"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8,
                      "completion_tokens_details": {"reasoning_tokens": 1}}});
        let m = merge(vec![a, b]);
        assert_eq!(m["id"], "a");
        assert_eq!(m["choices"][1]["index"], 1);
        assert_eq!(m["choices"][1]["message"]["content"], "two");
        assert_eq!(m["usage"]["prompt_tokens"], 10);
        assert_eq!(m["usage"]["total_tokens"], 14);
        assert_eq!(m["usage"]["completion_tokens_details"]["reasoning_tokens"], 3);
    }

    #[test]
    fn only_bounded_non_streaming_n_fans_out() {
        let p = |n: u64| {
            let v = json!({"model": "m", "n": n, "messages": []});
            let raw = Bytes::from(serde_json::to_vec(&v).unwrap());
            let skim = antirion_json::skim(&raw, &antirion_json::JsonLimits::default()).unwrap();
            Parsed::json(raw, skim, Some(v))
        };
        let on = json!({"n_fan_out": {"enabled": true, "max": 4}});
        assert_eq!(requested(&on, &p(3), false, false), Some(3));
        assert_eq!(requested(&on, &p(5), false, false), None);
        assert_eq!(requested(&on, &p(1), false, false), None);
        assert_eq!(requested(&on, &p(3), true, false), None);
        assert_eq!(requested(&on, &p(3), false, true), None);
        assert_eq!(requested(&json!({}), &p(3), false, false), None);
        assert!(without_n(&p(3)).unwrap().doc().unwrap().get("n").is_none());
    }
}

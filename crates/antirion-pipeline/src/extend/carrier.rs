//! One hook call on its extension's carrier: a webhook `POST`, a call into an extension compiled
//! into the binary, or a call into a Wasm component, each bounded by the extension's `timeout`.

use std::time::Instant;

use antirion_core::{Clock, SystemClock};
use antirion_ext::api::{self, Input};
use antirion_ext::{Carrier, HookAction, Plugin};
use http::{HeaderMap, HeaderName, HeaderValue};

use crate::ctx::Ctx;
use crate::external::{CallError, header_map, post_json};

/// Why a hook call failed.
pub(crate) enum Failure {
    Timeout,
    Failed(String),
}

impl Failure {
    /// The result label of metrics and the extension's statistics.
    pub(crate) fn result(&self) -> &'static str {
        match self {
            Failure::Timeout => "timeout",
            Failure::Failed(_) => "error",
        }
    }

    pub(crate) fn text(&self) -> String {
        match self {
            Failure::Timeout => "timeout".to_owned(),
            Failure::Failed(w) => w.clone(),
        }
    }
}

/// A call to an extension that runs in the process (built-in or Wasm); `None` for a webhook.
pub(crate) async fn in_process(p: &Plugin, input: &Input<'_>) -> Option<Result<HookAction, Failure>> {
    match &p.carrier {
        Carrier::Webhook { .. } => None,
        Carrier::Builtin(ext) => {
            let started = Instant::now();
            let r = api::call_guarded(ext.as_ref(), input);
            Some(if started.elapsed() > p.timeout {
                Err(Failure::Timeout)
            } else {
                r.map_err(|f| Failure::Failed(f.message().to_owned()))
                    .and_then(|a| {
                        antirion_ext::hook::check_action(input.hook, &a)
                            .map(|()| a)
                            .map_err(Failure::Failed)
                    })
            })
        }
        Carrier::Wasm(m) => {
            let payload = antirion_ext::hook::payload_of(input, p.include_body, &[]);
            let Ok(payload) = String::from_utf8(payload) else {
                return Some(Err(Failure::Failed("the payload is not UTF-8".into())));
            };
            Some(
                match tokio::time::timeout(p.timeout, m.call(input.hook, payload)).await {
                    Err(_) => Err(Failure::Timeout),
                    Ok(Err(f)) => Err(Failure::Failed(f.message().to_owned())),
                    Ok(Ok(answer)) => antirion_ext::parse_answer(input.hook, 200, answer.as_bytes())
                        .map_err(Failure::Failed),
                },
            )
        }
    }
}

/// One hook call of a request, on any carrier.
pub(crate) async fn invoke(cx: &Ctx, p: &Plugin, input: &Input<'_>) -> Result<HookAction, Failure> {
    if let Some(r) = in_process(p, input).await {
        return r;
    }
    let Carrier::Webhook {
        url,
        headers,
        signing_secret,
        max_response_bytes,
        forward_headers,
    } = &p.carrier
    else {
        return Ok(HookAction::Continue);
    };
    let payload = antirion_ext::hook::payload_of(input, p.include_body, forward_headers);
    let mut head: HeaderMap = header_map(headers);
    head.insert(
        HeaderName::from_static("x-antirion-hook"),
        HeaderValue::from_static(input.hook.as_str()),
    );
    let now = SystemClock.now().0 / 1_000_000;
    if let Some(sig) = signing_secret
        .as_ref()
        .map(antirion_core::Secret::expose)
        .and_then(|s| antirion_ext::signature(s, &payload, now))
        .and_then(|s| HeaderValue::from_str(&s).ok())
    {
        head.insert(HeaderName::from_static("x-antirion-signature"), sig);
    }
    let sent = tokio::time::timeout(
        p.timeout,
        post_json(
            &cx.rt,
            &cx.snap,
            &cx.request_id,
            crate::external::Post {
                url,
                headers: head,
                body: payload,
                timeout: p.timeout,
                max: *max_response_bytes,
            },
        ),
    )
    .await;
    let (status, answer) = match sent {
        Err(_) => return Err(Failure::Timeout),
        Ok(Err(CallError::TooLarge)) => {
            return Err(Failure::Failed(
                "the answer is larger than max_response_bytes".into(),
            ));
        }
        Ok(Err(e)) => return Err(Failure::Failed(e.text())),
        Ok(Ok(x)) => x,
    };
    antirion_ext::parse_answer(input.hook, status, &answer).map_err(Failure::Failed)
}

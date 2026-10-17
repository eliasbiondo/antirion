//! The upstream exchanges of a non-streaming hedge race. Each runs on a task of its own, so an
//! attempt that loses the race can be dropped at once while its exchange goes on: for up to
//! [`GRACE`] after the loss its answer is still awaited, so that the usage it reports is charged on
//! the attempt's own ledger line and the record keeps the attempt's real end. When the grace runs
//! out the exchange is aborted, which closes its connection. The request's record is settled once
//! every such tail has ended.

use std::sync::Arc;
use std::time::Duration;

use antirion_egress::{InboundResponse, OutboundRequest, TransportError};
use antirion_limits::ledger::{LineStatus, Purpose};
use antirion_obs::trace::SpanId;
use antirion_providers::DeploymentRt;
use antirion_runtime::{Runtime, Snapshot};
use bytes::Bytes;
use http::{HeaderMap, StatusCode};
use http_body_util::{BodyExt, Full};
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::AbortHandle;

use crate::account::{Deferred, DeferredCall, settle_deferred};
use crate::outcome::{Unread, collect};
use crate::reqlog::Draft;

/// How long a lost attempt's exchange is still awaited.
pub(crate) const GRACE: Duration = Duration::from_secs(1);

/// What an exchange produced: the status, headers and whole body, or why there is none.
pub(crate) type Answer = Result<(StatusCode, HeaderMap, Result<Bytes, Unread>), TransportError>;

/// An upstream exchange running on a task of its own.
#[derive(Clone, Debug)]
pub(crate) struct Exchange {
    answer: watch::Receiver<Option<Arc<Answer>>>,
    abort: AbortHandle,
}

impl Exchange {
    /// Sends `request` and reads its whole body (at most `cap` bytes) on a task of its own.
    pub(crate) fn start(rt: &Runtime, request: OutboundRequest, cap: u64) -> Exchange {
        let (tx, answer) = watch::channel(None);
        let egress = rt.egress.clone();
        let task = rt.tasks.spawn(async move {
            let got = match egress.send(request).await {
                Ok(InboundResponse {
                    status,
                    headers,
                    body,
                    abort,
                }) => {
                    let body = collect(body, cap).await;
                    if let (Err(_), Some(abort)) = (&body, abort) {
                        Box::pin(abort.abort()).await;
                    }
                    Ok((status, headers, body))
                }
                Err(e) => Err(e),
            };
            let _ = tx.send(Some(Arc::new(got)));
        });
        Exchange {
            answer,
            abort: task.abort_handle(),
        }
    }

    /// The exchange's answer; `None` when its task ended without one.
    pub(crate) async fn answer(&self) -> Option<Arc<Answer>> {
        let mut rx = self.answer.clone();
        rx.wait_for(Option::is_some).await.ok().and_then(|v| v.clone())
    }

    /// Ends the exchange, closing its connection.
    pub(crate) fn abort(&self) {
        self.abort.abort();
    }
}

/// An answer as the outcome stage reads a response: the body is already whole.
pub(crate) fn response(status: StatusCode, headers: &HeaderMap, body: &Bytes) -> InboundResponse {
    InboundResponse {
        status,
        headers: headers.clone(),
        body: Full::new(body.clone())
            .map_err(|never| match never {})
            .boxed_unsync(),
        abort: None,
    }
}

/// A lost attempt whose exchange is still awaited.
pub(crate) struct Tail {
    pub(crate) rt: Arc<Runtime>,
    pub(crate) snap: Arc<Snapshot>,
    pub(crate) draft: Draft,
    pub(crate) deferred: Option<Deferred>,
    pub(crate) deployment: Arc<DeploymentRt>,
    pub(crate) span: SpanId,
    pub(crate) purpose: Purpose,
    pub(crate) exchange: Exchange,
}

impl Tail {
    /// Waits for the exchange through the grace, then completes the attempt's record entry and
    /// writes its ledger line; the last tail of a request whose head is built settles its record.
    pub(crate) async fn run(self) {
        let answer = tokio::time::timeout(GRACE, self.exchange.answer()).await;
        let usage = match &answer {
            Ok(Some(a)) => usage_of(a),
            _ => {
                self.exchange.abort();
                None
            }
        };
        let end = std::time::Instant::now();
        let mut line = None;
        self.draft.update(|r| {
            if let Some((i, a)) = r
                .attempts
                .iter_mut()
                .enumerate()
                .find(|(_, a)| a.span == self.span)
            {
                a.end = end;
                if let Some((u, raw)) = &usage {
                    a.usage = Some(*u);
                    a.usage_raw = Some(raw.clone());
                }
                line = u32::try_from(i).ok();
            }
        });
        if let (Some(d), Some(line)) = (&self.deferred, line) {
            let (usage, raw) = usage.map_or((antirion_limits::usage::Usage::default(), None), |(u, raw)| {
                (u.normalised(), Some(raw))
            });
            let _ = settle_deferred(
                &self.rt,
                &self.snap,
                d,
                DeferredCall {
                    deployment: &self.deployment,
                    line,
                    usage,
                    raw,
                    batch: false,
                    purpose: self.purpose,
                    status: LineStatus::Cancelled,
                },
            )
            .await;
        }
        crate::reqlog::tail_ended(&self.rt, &self.draft);
    }
}

/// The usage an answer reported, as reported.
fn usage_of(a: &Answer) -> Option<(crate::reqlog::Usage, Value)> {
    let Ok((_, _, Ok(body))) = a else {
        return None;
    };
    let v: Value = serde_json::from_slice(body).ok()?;
    let reported = crate::apis::usage_of(&v);
    let raw = v.get("usage").or_else(|| v.get("usageMetadata"))?.clone();
    (!reported.is_empty()).then_some((reported, raw))
}

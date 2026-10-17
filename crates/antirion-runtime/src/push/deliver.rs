//! Sending push messages: one bounded queue per node, a tracked task per message that retries with
//! backoff what the push service may accept later (a connection failure, 429, 5xx), and drops the
//! subscription the push service says is gone (404, 410). Payloads, keys and endpoints are never
//! logged: a subscription is named by its id and its push service's origin.

use std::sync::Arc;
use std::time::Duration;

use antirion_egress::{OutboundRequest, TransportError, TrustClass};
use antirion_obs::metrics::{Fam, Metrics};
use http::header::{AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE, RETRY_AFTER};
use http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::{Semaphore, mpsc};
use tokio::time::Instant;

use super::vapid::Vapid;
use crate::Runtime;

/// Messages waiting to be sent, beyond which new ones are dropped.
const QUEUE: usize = 1024;
/// Messages being sent at once.
const IN_FLIGHT: usize = 8;
/// How long a message is offered to the push service, which drops it afterwards.
const TTL_SECS: u32 = 4 * 3600;
/// How long a message is retried.
const RETRY_FOR: Duration = Duration::from_secs(3600);
const FIRST_RETRY: Duration = Duration::from_secs(2);
const LONGEST_RETRY: Duration = Duration::from_secs(300);
const TIMEOUT: Duration = Duration::from_secs(15);

/// One encrypted message for one subscription.
pub(crate) struct Job {
    pub(crate) id: String,
    pub(crate) origin: String,
    pub(crate) endpoint: String,
    pub(crate) body: Vec<u8>,
    pub(crate) urgency: &'static str,
}

/// The node's push queue and the cluster's VAPID key, once read.
pub struct Pusher {
    tx: mpsc::Sender<Job>,
    rx: Mutex<Option<mpsc::Receiver<Job>>>,
    pub(crate) vapid: std::sync::OnceLock<Arc<Vapid>>,
}

impl Default for Pusher {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel(QUEUE);
        Pusher {
            tx,
            rx: Mutex::new(Some(rx)),
            vapid: std::sync::OnceLock::new(),
        }
    }
}

impl std::fmt::Debug for Pusher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pusher").finish_non_exhaustive()
    }
}

impl Pusher {
    pub(crate) fn enqueue(&self, job: Job, metrics: &Metrics) {
        if self.tx.try_send(job).is_err() {
            metrics.add(Fam::PushSent, &["dropped"], 1.0);
        }
    }
}

/// How one send went.
enum Sent {
    Delivered,
    /// The push service no longer knows the subscription.
    Gone(u16),
    /// Worth sending again, after the push service's `Retry-After` when it gave one.
    Later(String, Option<Duration>),
    Refused(String),
}

/// What a test send reports per subscription.
#[derive(Clone, Debug, Serialize)]
pub struct TestOutcome {
    pub id: String,
    pub origin: String,
    pub ok: bool,
    pub status: Option<u16>,
    pub error: Option<String>,
    /// The push service said the subscription is gone, so it was removed.
    pub removed: bool,
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|s| Duration::from_secs(s).min(LONGEST_RETRY))
}

fn classify(status: StatusCode, headers: &HeaderMap) -> (Sent, Option<u16>) {
    let code = status.as_u16();
    let sent = match code {
        200..=299 => Sent::Delivered,
        404 | 410 => Sent::Gone(code),
        429 | 500..=599 => Sent::Later(format!("the push service answered {code}"), retry_after(headers)),
        _ => Sent::Refused(format!("the push service refused the message with {code}")),
    };
    (sent, Some(code))
}

async fn send(rt: &Runtime, job: &Job) -> (Sent, Option<u16>) {
    let snap = rt.gens.load();
    let Some(tls) = snap.security.egress_tls.clone() else {
        return (Sent::Later("no TLS profile for egress".to_owned(), None), None);
    };
    let Ok(uri) = job.endpoint.parse::<Uri>() else {
        return (Sent::Refused("the endpoint is not a URL".to_owned()), None);
    };
    let vapid = match rt.vapid().await {
        Ok(v) => v,
        Err(e) => return (Sent::Later(e.to_string(), None), None),
    };
    let Ok(authorization) = vapid.authorization(&job.origin, rt.push_subject().as_deref(), rt.now_secs())
    else {
        return (
            Sent::Later("the VAPID token could not be signed".to_owned(), None),
            None,
        );
    };
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_ENCODING, HeaderValue::from_static("aes128gcm"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    headers.insert("ttl", HeaderValue::from(TTL_SECS));
    headers.insert("urgency", HeaderValue::from_static(job.urgency));
    match HeaderValue::from_str(&authorization) {
        Ok(mut v) => {
            v.set_sensitive(true);
            headers.insert(AUTHORIZATION, v);
        }
        Err(_) => return (Sent::Refused("the VAPID header is not valid".to_owned()), None),
    }
    let request = OutboundRequest {
        method: Method::POST,
        uri,
        headers,
        body: bytes::Bytes::from(job.body.clone()).into(),
        response_timeout: TIMEOUT,
        connect_timeout: None,
        fresh_connection: false,
        trust: TrustClass::ClientInfluenced,
        tls,
        policy: Arc::clone(&snap.security.egress),
        resend_when_closed: false,
    };
    match rt.egress.send(request).await {
        Ok(resp) => classify(resp.status, &resp.headers),
        Err(TransportError::EgressDenied(d)) => (
            Sent::Refused(format!("the endpoint is denied by the egress rules: {d}")),
            None,
        ),
        Err(e) => (Sent::Later(e.to_string(), None), None),
    }
}

/// Sends one message once and records the outcome on its subscription.
pub(crate) async fn once(rt: &Runtime, job: &Job) -> TestOutcome {
    let (sent, status) = send(rt, job).await;
    let (ok, error, removed) = settle(rt, job, &sent).await;
    TestOutcome {
        id: job.id.clone(),
        origin: job.origin.clone(),
        ok,
        status,
        error,
        removed,
    }
}

/// Records a final outcome: `(delivered, error, removed)`.
async fn settle(rt: &Runtime, job: &Job, sent: &Sent) -> (bool, Option<String>, bool) {
    match sent {
        Sent::Delivered => {
            rt.metrics.add(Fam::PushSent, &["sent"], 1.0);
            rt.note_delivery(&job.id, None).await;
            (true, None, false)
        }
        Sent::Gone(code) => {
            rt.metrics.add(Fam::PushSent, &["gone"], 1.0);
            tracing::info!(subscription = %job.id, origin = %job.origin, status = code, "a push subscription is gone; removed");
            let removed = rt
                .store
                .delete_record(super::SUBSCRIPTION, &job.id)
                .await
                .unwrap_or(false);
            (
                false,
                Some(format!(
                    "the push service answered {code}: the subscription is gone"
                )),
                removed,
            )
        }
        Sent::Later(why, _) | Sent::Refused(why) => {
            rt.metrics.add(Fam::PushSent, &["failed"], 1.0);
            tracing::warn!(subscription = %job.id, origin = %job.origin, reason = %why, "a push message was not delivered");
            rt.note_delivery(&job.id, Some(why.clone())).await;
            (false, Some(why.clone()), false)
        }
    }
}

async fn deliver(rt: Arc<Runtime>, job: Job) {
    let first = Instant::now();
    let mut backoff = FIRST_RETRY;
    loop {
        let (sent, _) = send(&rt, &job).await;
        if let Sent::Later(why, after) = &sent {
            let wait = after.unwrap_or(backoff);
            if first.elapsed() + wait < RETRY_FOR {
                rt.metrics.add(Fam::PushSent, &["retried"], 1.0);
                tracing::info!(subscription = %job.id, origin = %job.origin, reason = %why, "a push message is retried");
                tokio::select! {
                    () = rt.shutdown.cancelled() => return,
                    () = tokio::time::sleep(wait) => {}
                }
                backoff = (backoff * 2).min(LONGEST_RETRY);
                continue;
            }
        }
        settle(&rt, &job, &sent).await;
        return;
    }
}

/// Sends queued messages until shutdown, a few at a time, each in a task of the runtime's tracker.
pub async fn run(rt: Arc<Runtime>) {
    let Some(mut rx) = rt.state.console.push.rx.lock().take() else {
        return;
    };
    let slots = Arc::new(Semaphore::new(IN_FLIGHT));
    loop {
        let job = tokio::select! {
            () = rt.shutdown.cancelled() => return,
            next = rx.recv() => match next {
                Some(job) => job,
                None => return,
            },
        };
        let Ok(permit) = Arc::clone(&slots).acquire_owned().await else {
            return;
        };
        let task_rt = Arc::clone(&rt);
        rt.tasks.spawn(async move {
            deliver(task_rt, job).await;
            drop(permit);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_decide_what_happens_next() {
        let none = HeaderMap::new();
        assert!(matches!(classify(StatusCode::CREATED, &none).0, Sent::Delivered));
        assert!(matches!(classify(StatusCode::GONE, &none).0, Sent::Gone(410)));
        assert!(matches!(
            classify(StatusCode::NOT_FOUND, &none).0,
            Sent::Gone(404)
        ));
        assert!(matches!(
            classify(StatusCode::BAD_REQUEST, &none).0,
            Sent::Refused(_)
        ));
        let mut later = HeaderMap::new();
        later.insert(RETRY_AFTER, HeaderValue::from_static("30"));
        assert!(matches!(
            classify(StatusCode::TOO_MANY_REQUESTS, &later).0,
            Sent::Later(_, Some(d)) if d == Duration::from_secs(30)
        ));
        assert!(matches!(
            classify(StatusCode::SERVICE_UNAVAILABLE, &none).0,
            Sent::Later(_, None)
        ));
    }
}

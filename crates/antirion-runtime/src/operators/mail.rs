//! The outgoing mail queue: bounded, sent by one tracked worker at most `max_messages_per_minute`,
//! each message retried for up to an hour while its failures are temporary. Queuing never waits, so
//! no Admin API response waits for a mail server.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use antirion_egress::{EgressPolicy, TrustClass};
use antirion_mail::{MailError, Message, Server};
use antirion_obs::metrics::{Fam, Metrics};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::settings::MailSettings;

/// Messages waiting to be sent, beyond which new ones are dropped.
const QUEUE: usize = 1024;
/// How long a message is retried.
const RETRY_FOR: Duration = Duration::from_secs(3600);
const FIRST_RETRY: Duration = Duration::from_secs(2);
const LONGEST_RETRY: Duration = Duration::from_secs(300);

/// One message and the settings it is sent with.
#[derive(Debug)]
pub struct Outgoing {
    pub settings: MailSettings,
    pub message: Message,
    /// The serving egress rules, which decide the server addresses that may be connected to.
    pub policy: Arc<EgressPolicy>,
}

/// The node's mail queue; its worker starts with the first message.
#[derive(Debug, Default)]
pub struct Mailer {
    tx: Mutex<Option<mpsc::Sender<Outgoing>>>,
}

/// The connection settings of a message: the mail server is operator traffic under the egress
/// rules, checked on each address the host resolves to.
#[must_use]
pub fn server(s: &MailSettings, helo_name: &str, policy: &Arc<EgressPolicy>) -> Server {
    let (host, policy) = (s.host.clone(), Arc::clone(policy));
    Server {
        allowed: Some(Arc::new(move |ip| {
            antirion_egress::ssrf::check_addr(ip, &host, TrustClass::Operator, &policy).is_ok()
        })),
        host: s.host.clone(),
        port: s.port,
        security: s.security,
        username: s.username.clone(),
        password: s.password.clone(),
        timeout: s.timeout,
        helo_name: helo_name.to_owned(),
        tls: antirion_mail::default_tls(),
    }
}

impl Mailer {
    /// Queues a message; false (and counted as dropped) when the queue is full.
    pub fn enqueue(
        &self,
        out: Outgoing,
        metrics: &Metrics,
        tasks: &TaskTracker,
        shutdown: &CancellationToken,
    ) -> bool {
        let mut guard = self.tx.lock();
        let tx = guard.get_or_insert_with(|| {
            let (tx, rx) = mpsc::channel(QUEUE);
            tasks.spawn(worker(rx, metrics.clone(), shutdown.clone()));
            tx
        });
        if tx.try_send(out).is_ok() {
            true
        } else {
            metrics.add(Fam::EmailSent, &["dropped"], 1.0);
            false
        }
    }
}

/// Sends one message now and reports how it went: the SMTP test action.
pub async fn send_now(out: &Outgoing, metrics: &Metrics) -> Result<Duration, MailError> {
    let started = Instant::now();
    let result = antirion_mail::send(&server(&out.settings, "antirion", &out.policy), &out.message).await;
    metrics.add(
        Fam::EmailSent,
        &[if result.is_ok() { "sent" } else { "failed" }],
        1.0,
    );
    result.map(|()| started.elapsed())
}

async fn worker(mut rx: mpsc::Receiver<Outgoing>, metrics: Metrics, shutdown: CancellationToken) {
    let sent = Mutex::new(VecDeque::new());
    loop {
        let out = tokio::select! {
            () = shutdown.cancelled() => return,
            next = rx.recv() => match next {
                Some(out) => out,
                None => return,
            },
        };
        let first = Instant::now();
        let mut backoff = FIRST_RETRY;
        loop {
            if let Some(wait) = pace(&sent, out.settings.per_minute) {
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = tokio::time::sleep(wait) => {}
                }
            }
            sent.lock().push_back(Instant::now());
            match antirion_mail::send(&server(&out.settings, "antirion", &out.policy), &out.message).await {
                Ok(()) => {
                    metrics.add(Fam::EmailSent, &["sent"], 1.0);
                    break;
                }
                Err(e) if e.is_transient() && first.elapsed() + backoff < RETRY_FOR => {
                    tracing::info!(error = %e, "a mail could not be sent yet; retrying");
                    metrics.add(Fam::EmailSent, &["retried"], 1.0);
                    tokio::select! {
                        () = shutdown.cancelled() => return,
                        () = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(LONGEST_RETRY);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "a mail could not be sent");
                    metrics.add(Fam::EmailSent, &["failed"], 1.0);
                    break;
                }
            }
        }
    }
}

/// How long to wait before the next send so that no minute holds more than `per_minute` sends.
fn pace(sent: &Mutex<VecDeque<Instant>>, per_minute: u32) -> Option<Duration> {
    let mut sent = sent.lock();
    let now = Instant::now();
    while sent
        .front()
        .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
    {
        sent.pop_front();
    }
    let cap = usize::try_from(per_minute.max(1)).unwrap_or(usize::MAX);
    if sent.len() < cap {
        return None;
    }
    sent.front()
        .map(|oldest| Duration::from_secs(60).saturating_sub(now.duration_since(*oldest)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacing_keeps_a_minute_under_the_cap() {
        let sent = Mutex::new(VecDeque::new());
        assert!(pace(&sent, 2).is_none());
        let now = Instant::now();
        sent.lock().push_back(now);
        sent.lock().push_back(now);
        let wait = pace(&sent, 2).unwrap();
        assert!(wait <= Duration::from_secs(60) && wait > Duration::from_secs(59));
        let old = now.checked_sub(Duration::from_secs(61)).unwrap();
        *sent.lock() = VecDeque::from([old, old]);
        assert!(pace(&sent, 2).is_none());
        assert!(sent.lock().is_empty());
    }
}

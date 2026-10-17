//! Draining: taking the node out of service, by a stop signal or through the Admin API, while the
//! work it already accepted finishes or is cut cleanly at the deadline.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use antirion_config::scalar::parse_duration;
use antirion_config::views::ShutdownView;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Reasons of the drain rules.
pub mod reasons {
    antirion_error::reasons! {
        NODE_DRAINING = "node_draining" => Draining,
        DRAIN_DEADLINE = "drain_deadline" => Draining,
    }
}

/// `server.shutdown`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub drain_min: Duration,
    pub drain_max: Duration,
    pub flush_timeout: Duration,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            drain_min: Duration::from_secs(5),
            drain_max: Duration::from_secs(300),
            flush_timeout: Duration::from_secs(10),
        }
    }
}

impl Settings {
    #[must_use]
    pub fn from_view(v: &ShutdownView) -> Settings {
        let d = Settings::default();
        let get =
            |s: &Option<String>, default: Duration| s.as_deref().and_then(parse_duration).unwrap_or(default);
        let drain_min = get(&v.drain_min, d.drain_min);
        Settings {
            drain_min,
            drain_max: get(&v.drain_max, d.drain_max).max(drain_min),
            flush_timeout: get(&v.flush_timeout, d.flush_timeout),
        }
    }
}

/// Who started the drain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A stop signal: new requests are served for `drain_min`, then refused.
    Signal,
    /// The Admin API: new requests are refused at once and the process stays up.
    Api,
}

#[derive(Clone, Copy, Debug)]
struct Started {
    at: Instant,
    origin: Origin,
}

/// The node's drain state and the data-plane requests it waits for.
#[derive(Debug)]
pub struct Drain {
    pub settings: Settings,
    started: Mutex<Option<Started>>,
    /// Cancelled when the drain starts: readiness fails and connections are asked to close.
    pub begun: CancellationToken,
    /// Cancelled when the listeners stop accepting connections.
    pub closed: CancellationToken,
    /// Cancelled at the drain deadline: work still running is ended with `drain_deadline`.
    pub cut: CancellationToken,
    active: AtomicU64,
    idle: Notify,
}

/// One data-plane request in progress; dropping it ends the count.
#[derive(Debug)]
pub struct Active(Arc<Drain>);

impl Drop for Active {
    fn drop(&mut self) {
        if self.0.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

impl Drain {
    #[must_use]
    pub fn new(settings: Settings) -> Drain {
        Drain {
            settings,
            started: Mutex::new(None),
            begun: CancellationToken::new(),
            closed: CancellationToken::new(),
            cut: CancellationToken::new(),
            active: AtomicU64::new(0),
            idle: Notify::new(),
        }
    }

    /// Starts the drain; a drain already started keeps its start time. A signal after an API drain
    /// does not restart the `drain_min` grace.
    pub fn start(&self, origin: Origin) {
        let mut s = self.started.lock();
        if s.is_none() {
            *s = Some(Started {
                at: Instant::now(),
                origin,
            });
        }
        drop(s);
        self.begun.cancel();
    }

    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.begun.is_cancelled()
    }

    /// Who started the drain, if it started.
    #[must_use]
    pub fn origin(&self) -> Option<Origin> {
        self.started.lock().map(|s| s.origin)
    }

    /// When the drain started.
    #[must_use]
    pub fn started_at(&self) -> Option<Instant> {
        self.started.lock().map(|s| s.at)
    }

    /// When work still running is cut: `drain_max` after the start.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.started_at().map(|at| at + self.settings.drain_max)
    }

    /// Whether a new data-plane request is refused with `node_draining`.
    #[must_use]
    pub fn refuses_new(&self) -> bool {
        match *self.started.lock() {
            None => false,
            Some(Started {
                origin: Origin::Api, ..
            }) => true,
            Some(Started { at, .. }) => at.elapsed() >= self.settings.drain_min || self.closed.is_cancelled(),
        }
    }

    /// Whether the deadline passed and the work still running was cut.
    #[must_use]
    pub fn is_cut(&self) -> bool {
        self.cut.is_cancelled()
    }

    /// Counts a data-plane request until the guard is dropped.
    #[must_use]
    pub fn enter(self: &Arc<Self>) -> Active {
        self.active.fetch_add(1, Ordering::AcqRel);
        Active(Arc::clone(self))
    }

    /// Data-plane requests in progress.
    #[must_use]
    pub fn active(&self) -> u64 {
        self.active.load(Ordering::Acquire)
    }

    /// Returns once no data-plane request is in progress.
    pub async fn wait_idle(&self) {
        loop {
            let notified = self.idle.notified();
            if self.active() == 0 {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_api_drain_refuses_at_once_and_a_signal_drain_after_drain_min() {
        let api = Drain::new(Settings::default());
        assert!(!api.refuses_new());
        api.start(Origin::Api);
        assert!(api.is_draining() && api.refuses_new());
        api.start(Origin::Signal);
        assert_eq!(api.origin(), Some(Origin::Api));

        let signal = Drain::new(Settings {
            drain_min: Duration::from_millis(30),
            ..Settings::default()
        });
        signal.start(Origin::Signal);
        assert!(!signal.refuses_new());
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(signal.refuses_new());
    }

    #[tokio::test]
    async fn wait_idle_returns_when_the_last_request_ends() {
        let tasks = tokio_util::task::TaskTracker::new();
        let d = Arc::new(Drain::new(Settings::default()));
        let guard = d.enter();
        let waiter = {
            let d = Arc::clone(&d);
            tasks.spawn(async move { d.wait_idle().await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!waiter.is_finished());
        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn drain_max_is_never_below_drain_min() {
        let s = Settings::from_view(&ShutdownView {
            drain_min: Some("10s".into()),
            drain_max: Some("1s".into()),
            flush_timeout: None,
        });
        assert_eq!(s.drain_max, Duration::from_secs(10));
    }
}

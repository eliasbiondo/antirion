//! The stop sequence: a stop signal drains the node (readiness fails at once, new connections are
//! served for `drain_min`, then the listeners close), and the node stops once nothing is in flight
//! or at `drain_max` after the drain began, when the work still running is cut cleanly. A second
//! signal cuts at once.

use std::sync::Arc;
use std::time::Duration;

use antirion_runtime::Runtime;
use antirion_runtime::drain::Origin;
use tokio::time::Instant;

/// The stop signals: `SIGTERM` and `SIGINT`.
pub(crate) struct Signals {
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
}

impl Signals {
    pub(crate) fn new() -> anyhow::Result<Signals> {
        Ok(Signals {
            #[cfg(unix)]
            term: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
        })
    }

    async fn recv(&mut self) {
        #[cfg(unix)]
        tokio::select! {
            _ = self.term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn far() -> Instant {
    Instant::now() + Duration::from_secs(86_400 * 365)
}

/// Runs until the node must stop, then cancels the runtime's shutdown token.
pub(crate) async fn run(rt: Arc<Runtime>, mut signals: Signals) {
    let d = Arc::clone(&rt.drain);
    // Until a stop signal: an API-initiated drain cuts what remains at its deadline, and the
    // process stays up.
    loop {
        let deadline = d.deadline();
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = signals.recv() => break,
            () = d.begun.cancelled(), if deadline.is_none() => {}
            () = tokio::time::sleep_until(deadline.unwrap_or_else(far)), if deadline.is_some() && !d.is_cut() => {
                d.cut.cancel();
            }
        }
    }
    let by_api = d.origin() == Some(Origin::Api);
    tracing::info!("stop signal: draining");
    d.start(Origin::Signal);
    let started = d.started_at().unwrap_or_else(Instant::now);
    let deadline = d.deadline().unwrap_or_else(Instant::now);
    let mut forced = false;
    if !by_api {
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = tokio::time::sleep_until(started + d.settings.drain_min) => {}
            () = signals.recv() => forced = true,
        }
    }
    d.closed.cancel();
    if !forced {
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = d.wait_idle() => {}
            () = tokio::time::sleep_until(deadline) => {}
            () = signals.recv() => {}
        }
    }
    if d.active() > 0 {
        tracing::info!(
            in_flight = d.active(),
            "drain deadline: ending the requests still running"
        );
        d.cut.cancel();
        if tokio::time::timeout(d.settings.flush_timeout, d.wait_idle())
            .await
            .is_err()
        {
            tracing::warn!("requests were still running after the drain deadline");
        }
    }
    tracing::info!("stopping");
    rt.shutdown.cancel();
}

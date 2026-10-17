//! The timers of a passthrough mount's exchange: the wait for the provider's answer is bounded by
//! the deadline and the client's read timeout; a streamed answer by `stream_idle` (capped by the
//! read timeout) between chunks and by `stream_total` from the first byte relayed; any other answer
//! by the read timeout between chunks. A mount is not routed, so its exchanges keep no health.

use std::time::Duration;

use antirion_core::Stage;
use antirion_error::{GatewayError, Message};
use antirion_resilience::{Timeouts, format_duration};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use tokio::time::Instant;

use crate::ctx::BoxError;

/// The bounds of one mount exchange.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MountTimers {
    /// The longest wait for the answer's head, and the timer that sets it.
    pub(crate) head: Option<(Duration, &'static str)>,
    stream_idle: Option<Duration>,
    stream_total: Option<Duration>,
    read: Option<Duration>,
}

/// A relayed answer cut by its timer.
#[derive(Debug, thiserror::Error)]
#[error("the upstream answer exceeded its time limit")]
struct Cut;

fn shortest(a: Option<Duration>, b: Option<Duration>) -> Option<Duration> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

impl MountTimers {
    pub(crate) fn new(t: &Timeouts, read: Option<Duration>) -> MountTimers {
        let head = match (t.deadline, read) {
            (Some(d), Some(r)) if r < d => Some((r, "read timeout")),
            (Some(d), _) => Some((d, "deadline")),
            (None, r) => r.map(|r| (r, "read timeout")),
        };
        MountTimers {
            head,
            stream_idle: shortest(t.stream_idle, read),
            stream_total: t.stream_total,
            read,
        }
    }

    /// The error when the answer's head did not come in time.
    pub(crate) fn late(&self) -> GatewayError {
        let (value, name) = self.head.unwrap_or((Duration::ZERO, "deadline"));
        GatewayError::with_reason(
            antirion_resilience::reasons::DEADLINE_EXCEEDED,
            Stage::Attempt,
            Message::built(format!(
                "upstream timeout: {name} {} exceeded",
                format_duration(value)
            )),
        )
    }

    /// The answer's body under its timers: a cut ends it with an error, which aborts the response.
    pub(crate) fn body<S>(
        &self,
        inner: S,
        stream: bool,
    ) -> impl Stream<Item = Result<Bytes, BoxError>> + Send + 'static
    where
        S: Stream<Item = Result<Bytes, BoxError>> + Send + Unpin + 'static,
    {
        let far = Instant::now() + Duration::from_secs(86_400 * 365);
        let (idle, total) = if stream {
            (self.stream_idle, self.stream_total)
        } else {
            (self.read, None)
        };
        let total_at = total.map_or(far, |t| Instant::now() + t);
        futures_util::stream::unfold(Some(inner), move |state| async move {
            let mut s = state?;
            let idle_at = idle.map_or(far, |d| Instant::now() + d);
            tokio::select! {
                item = s.next() => item.map(|i| (i, Some(s))),
                () = tokio::time::sleep_until(idle_at.min(total_at)) => Some((Err(Box::new(Cut) as BoxError), None)),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeouts() -> Timeouts {
        antirion_resilience::Policy::default().timeouts
    }

    #[test]
    fn the_read_timeout_caps_the_head_and_the_idle_timer() {
        let m = MountTimers::new(&timeouts(), Some(Duration::from_secs(1)));
        assert_eq!(m.head, Some((Duration::from_secs(1), "read timeout")));
        assert_eq!(m.stream_idle, Some(Duration::from_secs(1)));
        let none = MountTimers::new(&timeouts(), None);
        assert_eq!(none.head, Some((Duration::from_secs(900), "deadline")));
    }

    #[tokio::test]
    async fn stream_total_cuts_a_stream() {
        let mut t = timeouts();
        t.stream_total = Some(Duration::from_millis(50));
        let m = MountTimers::new(&t, None);
        let endless = futures_util::stream::pending::<Result<Bytes, BoxError>>();
        let items: Vec<_> = m.body(endless, true).collect().await;
        assert_eq!(items.len(), 1);
        assert!(items.first().is_some_and(Result::is_err));
    }
}

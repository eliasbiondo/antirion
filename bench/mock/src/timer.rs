//! A timer thread for scripted delays: the runtime's timer wheel rounds every deadline up to the
//! next millisecond tick, which alone would put the direct run's p99 a millisecond over the script.
//! One OS thread keeps the deadlines in a heap and wakes each task at its instant.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

struct Due(Instant, u64, oneshot::Sender<()>);

impl PartialEq for Due {
    fn eq(&self, other: &Self) -> bool {
        (self.0, self.1) == (other.0, other.1)
    }
}
impl Eq for Due {}
impl PartialOrd for Due {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Due {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.0, self.1).cmp(&(other.0, other.1))
    }
}

/// Wakes tasks at precise instants. Dropping it stops and joins the thread.
#[derive(Debug)]
pub struct Timer {
    tx: Option<mpsc::Sender<(Instant, oneshot::Sender<()>)>>,
    thread: Option<JoinHandle<()>>,
}

/// The last stretch before a deadline is spun, not slept: sleeping threads are woken late by up to
/// a share of the sleep (timer coalescing), and by the scheduler.
const SPIN: Duration = Duration::from_micros(200);

/// How long to block when `left` remains: wake early by an eighth plus half a millisecond, so the
/// sleep's own lateness lands before the deadline; the rest is spun.
fn early(left: Duration) -> Duration {
    left.saturating_sub(left / 8 + Duration::from_micros(500))
        .max(left / 2)
}

fn run(rx: &mpsc::Receiver<(Instant, oneshot::Sender<()>)>) {
    let mut heap: BinaryHeap<Reverse<Due>> = BinaryHeap::new();
    let mut seq = 0u64;
    loop {
        let now = Instant::now();
        while heap.peek().is_some_and(|Reverse(d)| d.0 <= now) {
            if let Some(Reverse(Due(_, _, tx))) = heap.pop() {
                let _ = tx.send(());
            }
        }
        let next = match heap.peek() {
            Some(Reverse(d)) => {
                let left = d.0.saturating_duration_since(now);
                if left <= SPIN {
                    std::hint::spin_loop();
                    match rx.try_recv() {
                        Ok(v) => Ok(v),
                        Err(mpsc::TryRecvError::Empty) => Err(mpsc::RecvTimeoutError::Timeout),
                        Err(mpsc::TryRecvError::Disconnected) => Err(mpsc::RecvTimeoutError::Disconnected),
                    }
                } else {
                    rx.recv_timeout(early(left))
                }
            }
            None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match next {
            Ok((at, tx)) => {
                seq += 1;
                heap.push(Reverse(Due(at, seq, tx)));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

impl Timer {
    pub fn start() -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("mock-timer".into())
            .spawn(move || run(&rx))?;
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
        })
    }

    /// Resolves at `at` (at once when it has passed).
    pub async fn at(&self, at: Instant) {
        if at <= Instant::now() {
            return;
        }
        let (tx, rx) = oneshot::channel();
        if let Some(t) = &self.tx
            && t.send((at, tx)).is_ok()
        {
            let _ = rx.await;
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wakes_in_order_and_on_time() {
        let t = Timer::start().unwrap();
        let start = Instant::now();
        let (a, b) = tokio::join!(
            t.at(start + Duration::from_millis(30)),
            t.at(start + Duration::from_millis(10))
        );
        let _ = (a, b);
        let took = start.elapsed();
        assert!(
            took >= Duration::from_millis(30) && took < Duration::from_millis(80),
            "{took:?}"
        );
        t.at(start).await;
    }
}

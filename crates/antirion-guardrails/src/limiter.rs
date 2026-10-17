//! `max_concurrency`: how many evaluations of one networked guardrail may be in flight on a node.
//! One more counts as a failure at once; nothing queues.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The bound of one guardrail.
#[derive(Debug)]
pub struct Limiter {
    max: usize,
    in_flight: Arc<AtomicUsize>,
}

/// One evaluation in flight, released on drop.
#[derive(Debug)]
pub struct Permit(Arc<AtomicUsize>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Limiter {
    #[must_use]
    pub fn new(max: usize) -> Limiter {
        Limiter {
            max: max.max(1),
            in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// A permit, or `None` when `max` evaluations are in flight.
    #[must_use]
    pub fn try_acquire(&self) -> Option<Permit> {
        self.in_flight
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.max).then_some(n + 1)
            })
            .ok()
            .map(|_| Permit(Arc::clone(&self.in_flight)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bound_holds_and_permits_release() {
        let l = Limiter::new(2);
        let a = l.try_acquire().unwrap();
        let _b = l.try_acquire().unwrap();
        assert!(l.try_acquire().is_none());
        drop(a);
        assert!(l.try_acquire().is_some());
    }
}

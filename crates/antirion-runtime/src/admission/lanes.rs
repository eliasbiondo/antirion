//! The admission queue. In FIFO mode every waiter shares one lane; with fairness by project each
//! project has its own lane and the lanes are served in turn, one request each (deficit round robin
//! with a quantum of one request), so a project's backlog delays another project's request by at
//! most one slot. Lanes keep their buffers once used, so a busy queue does not allocate.

use std::collections::{HashMap, VecDeque};

use tokio::sync::oneshot;

use super::Refused;

/// Lanes that are kept empty beyond this many are dropped.
const MAX_IDLE_LANES: usize = 4096;

pub(super) struct Waiter {
    pub(super) id: u64,
    /// The project's lane; FIFO mode ignores it.
    pub(super) lane: u64,
    pub(super) tx: oneshot::Sender<Result<(), Refused>>,
}

#[derive(Default)]
pub(super) struct Lanes {
    fair: bool,
    lanes: HashMap<u64, VecDeque<Waiter>>,
    /// The non-empty lanes in serving order; each appears once.
    ring: VecDeque<u64>,
    len: usize,
}

impl Lanes {
    pub(super) fn len(&self) -> usize {
        self.len
    }

    fn lane_of(&self, w: &Waiter) -> u64 {
        if self.fair { w.lane } else { 0 }
    }

    pub(super) fn push(&mut self, w: Waiter) {
        let lane = self.lane_of(&w);
        let q = self.lanes.entry(lane).or_default();
        if q.is_empty() {
            self.ring.push_back(lane);
        }
        q.push_back(w);
        self.len += 1;
    }

    /// The next waiter to serve: the head of the lane whose turn it is.
    pub(super) fn pop(&mut self) -> Option<Waiter> {
        while let Some(lane) = self.ring.pop_front() {
            let Some(q) = self.lanes.get_mut(&lane) else {
                continue;
            };
            let Some(w) = q.pop_front() else {
                continue;
            };
            if !q.is_empty() {
                self.ring.push_back(lane);
            }
            self.len -= 1;
            self.trim();
            return Some(w);
        }
        None
    }

    /// Removes the waiter `id` of `lane`; false when it is not queued.
    pub(super) fn remove(&mut self, id: u64, lane: u64) -> bool {
        let lane = if self.fair { lane } else { 0 };
        let Some(q) = self.lanes.get_mut(&lane) else {
            return false;
        };
        let Some(pos) = q.iter().position(|w| w.id == id) else {
            return false;
        };
        q.remove(pos);
        if q.is_empty() {
            self.ring.retain(|l| *l != lane);
        }
        self.len -= 1;
        true
    }

    /// Removes the waiter that arrived last, whatever its lane.
    pub(super) fn pop_newest(&mut self) -> Option<Waiter> {
        let lane = self
            .ring
            .iter()
            .filter_map(|l| Some((*l, self.lanes.get(l)?.back()?.id)))
            .max_by_key(|(_, id)| *id)
            .map(|(l, _)| l)?;
        let q = self.lanes.get_mut(&lane)?;
        let w = q.pop_back()?;
        if q.is_empty() {
            self.ring.retain(|l| *l != lane);
        }
        self.len -= 1;
        Some(w)
    }

    /// Switches between FIFO and fairness by project; queued requests keep their arrival order within
    /// their lane.
    pub(super) fn set_fair(&mut self, fair: bool) {
        if self.fair == fair {
            return;
        }
        let mut all: Vec<Waiter> = self.lanes.drain().flat_map(|(_, q)| q).collect();
        all.sort_by_key(|w| w.id);
        self.ring.clear();
        self.len = 0;
        self.fair = fair;
        for w in all {
            self.push(w);
        }
    }

    fn trim(&mut self) {
        if self.lanes.len() > MAX_IDLE_LANES {
            self.lanes.retain(|_, q| !q.is_empty());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiter(id: u64, lane: u64) -> (Waiter, oneshot::Receiver<Result<(), Refused>>) {
        let (tx, rx) = oneshot::channel();
        (Waiter { id, lane, tx }, rx)
    }

    fn order(l: &mut Lanes) -> Vec<u64> {
        std::iter::from_fn(|| l.pop()).map(|w| w.id).collect()
    }

    fn fill(l: &mut Lanes, items: &[(u64, u64)]) -> Vec<oneshot::Receiver<Result<(), Refused>>> {
        items
            .iter()
            .map(|(id, lane)| {
                let (w, rx) = waiter(*id, *lane);
                l.push(w);
                rx
            })
            .collect()
    }

    #[test]
    fn fifo_keeps_arrival_order() {
        let mut l = Lanes::default();
        let _rx = fill(&mut l, &[(0, 1), (1, 1), (2, 2), (3, 1)]);
        assert_eq!(order(&mut l), [0, 1, 2, 3]);
    }

    #[test]
    fn projects_are_served_in_turn() {
        let mut l = Lanes::default();
        l.set_fair(true);
        let mut items: Vec<(u64, u64)> = (0..1000).map(|i| (i, 7)).collect();
        items.push((1000, 9));
        let _rx = fill(&mut l, &items);
        let got = order(&mut l);
        let pos = got.iter().position(|id| *id == 1000).unwrap();
        assert!(pos <= 1, "the lone request was served at {pos}");
        assert_eq!(got.len(), 1001);
    }

    #[test]
    fn removal_newest_and_switching_modes() {
        let mut l = Lanes::default();
        l.set_fair(true);
        let _rx = fill(&mut l, &[(0, 1), (1, 1), (2, 2), (3, 3), (4, 1)]);
        assert!(l.remove(3, 3));
        assert!(!l.remove(3, 3));
        assert_eq!(l.pop_newest().map(|w| w.id), Some(4));
        l.set_fair(false);
        assert_eq!(l.len(), 3);
        assert_eq!(order(&mut l), [0, 1, 2]);
        assert_eq!(l.len(), 0);
    }
}

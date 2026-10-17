//! Identical concurrent misses on one node: the first request to miss a key leads, and later ones
//! wait for it to finish (its response written, or found unstorable) before looking again. The
//! leader's [`Flight`] ends when its last clone is dropped, so a leader that fails or panics never
//! strands its followers.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::watch;

type Table = Mutex<HashMap<String, (u64, watch::Receiver<()>)>>;

/// The in-flight leaders of a node, by backend key.
#[derive(Debug, Default)]
pub struct Flights {
    table: Arc<Table>,
    next: AtomicU64,
}

/// A leader's hold on its key, released on drop.
#[derive(Debug)]
pub struct Flight {
    table: Arc<Table>,
    key: String,
    id: u64,
    _done: watch::Sender<()>,
}

impl Drop for Flight {
    fn drop(&mut self) {
        let mut table = self.table.lock();
        if table.get(&self.key).is_some_and(|(id, _)| *id == self.id) {
            table.remove(&self.key);
        }
    }
}

/// What a miss does.
#[derive(Debug)]
pub enum Join {
    /// No identical request is in flight: this one leads.
    Lead(Flight),
    /// One is: wait for it.
    Follow(Follower),
}

/// A request waiting for the leader of its key.
#[derive(Debug)]
pub struct Follower(watch::Receiver<()>);

impl Follower {
    /// Waits until the leader finished, at most `max_wait`; whether it did.
    pub async fn wait(mut self, max_wait: Duration) -> bool {
        tokio::time::timeout(max_wait, self.0.changed()).await.is_ok()
    }
}

impl Flights {
    /// Leads `key`, or follows its leader.
    pub fn join(&self, key: &str) -> Join {
        let mut table = self.table.lock();
        if let Some((_, rx)) = table.get(key) {
            return Join::Follow(Follower(rx.clone()));
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = watch::channel(());
        table.insert(key.to_owned(), (id, rx));
        Join::Lead(Flight {
            table: Arc::clone(&self.table),
            key: key.to_owned(),
            id,
            _done: tx,
        })
    }

    /// The number of keys being led.
    #[must_use]
    pub fn len(&self) -> usize {
        self.table.lock().len()
    }

    /// Whether no key is being led.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn followers_wake_when_the_leader_ends() {
        let f = Flights::default();
        let Join::Lead(lead) = f.join("k") else {
            panic!("first joins lead")
        };
        let Join::Follow(follower) = f.join("k") else {
            panic!("second follows")
        };
        let Join::Lead(_other) = f.join("other") else {
            panic!("other keys lead")
        };
        let (finished, ()) = tokio::join!(follower.wait(Duration::from_secs(5)), async move {
            tokio::task::yield_now().await;
            drop(lead);
        });
        assert!(finished);
        assert!(matches!(f.join("k"), Join::Lead(_)));
    }

    #[tokio::test]
    async fn a_follower_gives_up_after_max_wait() {
        let f = Flights::default();
        let _lead = f.join("k");
        let Join::Follow(follower) = f.join("k") else {
            panic!("follows")
        };
        assert!(!follower.wait(Duration::from_millis(5)).await);
    }
}

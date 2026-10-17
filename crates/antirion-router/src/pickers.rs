//! What the external endpoint pickers of this replica's pools answered: how many times each was
//! asked and failed, its last answer (the deployments it chose, or why it failed) and the deployment
//! that last served after one of its picks. Route state reports it per pool.

use std::collections::HashMap;

use antirion_core::UtcMicros;
use parking_lot::Mutex;

/// How one ask of a picker ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    /// The deployments the picker chose, in its order; empty for "no endpoint".
    Picked(Vec<String>),
    /// The picker was unreachable, timed out or answered malformedly.
    Failed(String),
}

/// A picker as route state shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PickerView {
    pub asks: u64,
    pub failures: u64,
    /// When it was last asked, and how that ended.
    pub last: Option<(UtcMicros, Asked)>,
    /// When it last answered, and the deployments it chose.
    pub last_pick: Option<(UtcMicros, Vec<String>)>,
    /// When it last failed, and why.
    pub last_failure: Option<(UtcMicros, String)>,
    /// The deployment that produced the terminal response after its last pick, when one did.
    pub last_served: Option<String>,
}

impl PickerView {
    /// `ok` when the last ask was answered, `failing` when it failed, `unknown` before any ask.
    #[must_use]
    pub fn state(&self) -> &'static str {
        match &self.last {
            None => "unknown",
            Some((_, Asked::Picked(_))) => "ok",
            Some((_, Asked::Failed(_))) => "failing",
        }
    }
}

/// The pickers of this replica, by the pool node's path.
#[derive(Debug, Default)]
pub struct Pickers {
    entries: Mutex<HashMap<String, PickerView>>,
}

impl Pickers {
    /// Records an ask of the picker of pool `node`.
    pub fn asked(&self, node: &str, at: UtcMicros, outcome: Asked) {
        let mut all = self.entries.lock();
        let v = all.entry(node.to_owned()).or_default();
        v.asks += 1;
        match &outcome {
            Asked::Picked(chosen) => {
                v.last_pick = Some((at, chosen.clone()));
                v.last_served = None;
            }
            Asked::Failed(why) => {
                v.failures += 1;
                v.last_failure = Some((at, why.clone()));
            }
        }
        v.last = Some((at, outcome));
    }

    /// Records the deployment that served after a pick of pool `node`.
    pub fn served(&self, node: &str, deployment: &str) {
        if let Some(v) = self.entries.lock().get_mut(node) {
            v.last_served = Some(deployment.to_owned());
        }
    }

    #[must_use]
    pub fn view(&self, node: &str) -> PickerView {
        self.entries.lock().get(node).cloned().unwrap_or_default()
    }

    pub fn reset(&self) {
        self.entries.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_failures_and_the_last_pick() {
        let p = Pickers::default();
        assert_eq!(p.view("r/pool").state(), "unknown");
        p.asked("r/pool", UtcMicros(1), Asked::Picked(vec!["b".into()]));
        p.served("r/pool", "b");
        p.asked(
            "r/pool",
            UtcMicros(2),
            Asked::Failed("the picker timed out".into()),
        );
        let v = p.view("r/pool");
        assert_eq!((v.asks, v.failures, v.state()), (2, 1, "failing"));
        assert_eq!(v.last_pick, Some((UtcMicros(1), vec!["b".to_owned()])));
        assert_eq!(v.last_served.as_deref(), Some("b"));
        p.asked("r/pool", UtcMicros(3), Asked::Picked(vec![]));
        let v = p.view("r/pool");
        assert_eq!(v.state(), "ok");
        assert_eq!(v.last_served, None, "a new pick has not served yet");
        assert_eq!(v.last_failure.map(|f| f.0), Some(UtcMicros(2)));
    }
}

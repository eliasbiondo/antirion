//! The health verdict of a stream after its commit. The attempt loop records a stream's success at
//! its commit; when the stream then fails (a reset, an idle timeout, a malformed event), that
//! failure takes the success's place. The relay may fail before the loop recorded the success, so
//! whichever comes second applies the failure.

use std::sync::{Arc, Mutex};

use antirion_resilience::Committed;
use antirion_runtime::Runtime;

#[derive(Debug, Default)]
enum Slot {
    #[default]
    Waiting,
    Committed(Committed),
    Failed,
    Done,
}

/// The post-commit state of a request's stream, shared by the attempt loop and the relay.
#[derive(Clone, Debug, Default)]
pub(crate) struct PostCommit(Arc<Mutex<Slot>>);

impl PostCommit {
    /// The loop recorded the success of the attempt that committed.
    pub(crate) fn committed(&self, rt: &Runtime, c: Committed) {
        let Ok(mut slot) = self.0.lock() else { return };
        match std::mem::take(&mut *slot) {
            Slot::Failed => {
                apply(rt, &c);
                *slot = Slot::Done;
            }
            _ => *slot = Slot::Committed(c),
        }
    }

    /// The committed stream failed.
    pub(crate) fn failed(&self, rt: &Runtime) {
        let Ok(mut slot) = self.0.lock() else { return };
        match std::mem::take(&mut *slot) {
            Slot::Committed(c) => {
                apply(rt, &c);
                *slot = Slot::Done;
            }
            Slot::Done => *slot = Slot::Done,
            Slot::Waiting | Slot::Failed => *slot = Slot::Failed,
        }
    }
}

fn apply(rt: &Runtime, c: &Committed) {
    let snap = rt.gens.load();
    let settings = snap.health.for_deployment(c.deployment());
    rt.state.health.failed_after_commit(c, settings, rt.now());
}

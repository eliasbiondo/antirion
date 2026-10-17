//! Keys and the people who hold them, beyond configuration: reminders before a key expires or is
//! due for rotation, the durable record of when keys were last used and users last signed in, and
//! key anomaly flags. Everything here runs off the request path.

use std::sync::Arc;

use antirion_core::RequestId;
use antirion_core::audit::{Actor, ActorKind, AuditEntry, AuditKind, Outcome};
use serde_json::Value;

use crate::Runtime;

pub mod anomalies;
pub mod marks;
pub mod reminders;

/// Starts the background tasks of this module.
pub fn spawn(rt: &Arc<Runtime>) {
    rt.tasks.spawn(reminders::run(Arc::clone(rt)));
    rt.tasks.spawn(marks::run(Arc::clone(rt)));
    rt.tasks.spawn(anomalies::run(Arc::clone(rt)));
}

impl Runtime {
    /// An audit entry for something the gateway noticed on its own (a reminder, a flag), so the
    /// activity feeds that read the audit log show it.
    pub(crate) async fn audit_system(&self, actor: &str, action: &str, resource: &str, changes: Value) {
        let generation = self.gens.load().generation;
        let entry = AuditEntry {
            id: RequestId::generate().as_str().to_owned(),
            time: self.now().to_rfc3339(),
            actor: Actor {
                kind: ActorKind::System,
                id: Some(actor.to_owned()),
            },
            action: action.to_owned(),
            resource: resource.to_owned(),
            outcome: Outcome::Success,
            status_code: 200,
            generation_before: Some(generation),
            generation_after: Some(generation),
            request_id: RequestId::generate().as_str().to_owned(),
            source_ip: None,
            changes,
            kind: AuditKind::Write,
            reason: None,
        };
        if let Err(e) = self.store.append_audit(entry).await {
            tracing::warn!(error = %e, action, "a system audit entry could not be written");
        }
    }
}

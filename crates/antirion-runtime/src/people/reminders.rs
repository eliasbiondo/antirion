//! `key.expiring` and `key.rotation_due`: a key's reminder days before its expiry or rotation due
//! date each send one event. When the clock passes several reminder days at once only the latest is
//! sent. One node sends each reminder: it is claimed in the store first.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_store::PendingRequest;
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::Runtime;
use crate::keypolicy::KeyPolicy;

/// The store kind that records a sent reminder.
const SENT: &str = "key_reminder";
/// The audit actor of reminders.
const ACTOR: &str = "key_reminders";
const SWEEP_EVERY: Duration = Duration::from_secs(1);
const DAY_US: i64 = 86_400_000_000;

/// The reminders this node has already claimed or seen claimed, so each is offered to the store
/// once.
#[derive(Debug, Default)]
pub struct Claimed {
    ids: Mutex<HashSet<String>>,
}

impl Claimed {
    /// Forgets every claim (a test reset clears the store's copy).
    pub fn clear(&self) {
        self.ids.lock().clear();
    }
}

/// What a reminder is about.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Kind {
    Expiring,
    RotationDue,
}

impl Kind {
    fn event(self) -> &'static str {
        match self {
            Kind::Expiring => "key.expiring",
            Kind::RotationDue => "key.rotation_due",
        }
    }

    fn audit_action(self) -> &'static str {
        match self {
            Kind::Expiring => "keys.expiring",
            Kind::RotationDue => "keys.rotation_due",
        }
    }
}

/// The reminder day to send at `now` for a date `at`: the smallest of `days` already reached. A due
/// date also reminds on the day itself (0); an expiry sends nothing once passed.
fn reached(days: &[u32], at: UtcMicros, now: UtcMicros, kind: Kind) -> Option<u32> {
    if kind == Kind::Expiring && now >= at {
        return None;
    }
    let extra = (kind == Kind::RotationDue).then_some(0);
    days.iter()
        .copied()
        .chain(extra)
        .filter(|d| now.0 >= at.0.saturating_sub(i64::from(*d).saturating_mul(DAY_US)))
        .min()
}

/// One reminder that is due now.
struct Due {
    kind: Kind,
    key: String,
    days: u32,
    at: UtcMicros,
    id: String,
    data: Value,
}

fn tenancy(doc: &Value, project: &str) -> (String, String) {
    let parent = |c: &str, n: &str, f: &str| {
        doc.get(c)
            .and_then(|m| m.get(n))
            .and_then(|s| s.get(f))
            .and_then(Value::as_str)
            .unwrap_or(antirion_config::model::DEFAULT_TENANT)
            .to_owned()
    };
    let team = parent("projects", project, "team");
    let org = parent("teams", &team, "organization");
    (team, org)
}

impl Runtime {
    /// The reminders due at `now` for every key of the serving configuration.
    fn due_reminders(&self, now: UtcMicros) -> Vec<Due> {
        let snap = self.gens.load();
        let doc = &snap.config.doc;
        let metas = snap.config.meta.resources.get("keys");
        let mut policies: HashMap<String, KeyPolicy> = HashMap::new();
        let mut out = Vec::new();
        for (name, spec) in doc.get("keys").and_then(Value::as_object).into_iter().flatten() {
            let Some(meta) = metas.and_then(|m| m.get(name)) else {
                continue;
            };
            let project = spec
                .get("project")
                .and_then(Value::as_str)
                .unwrap_or(antirion_config::model::DEFAULT_TENANT);
            let policy = policies
                .entry(project.to_owned())
                .or_insert_with(|| KeyPolicy::of(doc, project));
            let expiry = spec
                .get("expires_at")
                .and_then(Value::as_str)
                .and_then(|s| UtcMicros::parse_rfc3339(s).ok());
            let since = meta.rotated_at.as_deref().unwrap_or(&meta.created_at);
            let due = UtcMicros::parse_rfc3339(since)
                .ok()
                .and_then(|t| policy.rotation_due_at(t));
            for (kind, at) in [(Kind::Expiring, expiry), (Kind::RotationDue, due)] {
                let Some(at) = at else { continue };
                let Some(days) = reached(&policy.reminder_days, at, now, kind) else {
                    continue;
                };
                let (team, organization) = tenancy(doc, project);
                let date = if kind == Kind::Expiring {
                    "expires_at"
                } else {
                    "due_at"
                };
                out.push(Due {
                    kind,
                    key: name.clone(),
                    days,
                    at,
                    id: format!("{}/{}/{}/{}", kind.event(), meta.uid.to_hex(), at.0, days),
                    data: json!({
                        "key": name, "project": project, "team": team, "organization": organization,
                        "owner": spec.get("owner"), "days": days, date: at.to_rfc3339_seconds(),
                    }),
                });
            }
        }
        out
    }

    /// Sends each reminder that is due and that no node has claimed yet.
    pub async fn send_key_reminders(&self) {
        let now = self.now();
        for due in self.due_reminders(now) {
            if self.state.key_reminders.ids.lock().contains(&due.id) {
                continue;
            }
            let claimed = self
                .store
                .insert_request(PendingRequest {
                    kind: SENT.to_owned(),
                    id: due.id.clone(),
                    subject: due.key.clone(),
                    state: "sent".to_owned(),
                    author: ACTOR.to_owned(),
                    created_us: now.0,
                    version: 0,
                    body: due.data.to_string(),
                })
                .await;
            match claimed {
                Ok(won) => {
                    self.state.key_reminders.ids.lock().insert(due.id.clone());
                    if !won {
                        continue;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "a key reminder could not be claimed");
                    continue;
                }
            }
            let snap = self.gens.load();
            self.send_alert(&snap, None, due.kind.event(), due.data.clone());
            self.audit_system(
                ACTOR,
                due.kind.audit_action(),
                &format!("keys/{}", due.key),
                json!({"days": due.days, "at": due.at.to_rfc3339_seconds()}),
            )
            .await;
        }
    }
}

/// Sends key reminders every second until the node stops.
pub(crate) async fn run(rt: Arc<Runtime>) {
    loop {
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = tokio::time::sleep(SWEEP_EVERY) => {}
        }
        rt.send_key_reminders().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAYS: [u32; 2] = [7, 2];

    fn at_days(d: i64) -> UtcMicros {
        UtcMicros(d * DAY_US)
    }

    #[test]
    fn the_latest_reached_reminder_day_is_sent() {
        let expiry = at_days(10);
        assert_eq!(reached(&DAYS, expiry, at_days(2), Kind::Expiring), None);
        assert_eq!(reached(&DAYS, expiry, at_days(3), Kind::Expiring), Some(7));
        assert_eq!(reached(&DAYS, expiry, at_days(9), Kind::Expiring), Some(2));
        assert_eq!(reached(&DAYS, expiry, at_days(10), Kind::Expiring), None);
    }

    #[test]
    fn a_due_date_also_reminds_on_the_day() {
        let due = at_days(30);
        assert_eq!(reached(&DAYS, due, at_days(29), Kind::RotationDue), Some(2));
        assert_eq!(reached(&DAYS, due, at_days(31), Kind::RotationDue), Some(0));
    }
}

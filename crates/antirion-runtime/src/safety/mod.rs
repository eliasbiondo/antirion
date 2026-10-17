//! Safety events and guardrail statistics, off the request path: settlement queues a request's
//! events and its evaluations' counts; the request-log writer stores them in batches, then
//! announces the events that call for it (`guardrail.event`, `safety` alert instances, exposed-key
//! flags) from the node that recorded them. Reads write what is queued first.

pub mod event;
pub mod stats;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use antirion_obs::sketch::Sketch;
use antirion_store::{GuardrailHour, SafetyEventRow, StoreError};
use parking_lot::Mutex;
use serde_json::Value;

pub use event::Mark;

use crate::Runtime;

/// Events waiting to be written; past this the oldest are dropped.
const QUEUE_CAPACITY: usize = 50_000;
/// Events written per store call.
const BATCH: usize = 1_000;
/// Announcements waiting; past this the oldest are dropped.
const ANNOUNCE_CAPACITY: usize = 10_000;
const HOUR_US: i64 = 3_600_000_000;
/// The built-in alert rule critical events raise instances of.
pub const ALERT_RULE: &str = "safety_event";
/// The actor of what detection does by itself.
const ACTOR: &str = "system";

/// An event with what it asks for once stored.
#[derive(Clone, Debug)]
struct Queued {
    row: SafetyEventRow,
    notify: bool,
    exposed: Vec<String>,
}

/// One guardrail's evaluations in one hour, on this node.
#[derive(Clone, Debug, Default)]
struct Counts {
    evaluations: i64,
    hits: i64,
    blocks: i64,
    redactions: i64,
    flags: i64,
    errors: i64,
    latency: Sketch,
}

/// What one evaluation adds to its guardrail's hour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Tally {
    evaluations: i64,
    hits: i64,
    blocks: i64,
    redactions: i64,
    flags: i64,
    errors: i64,
}

impl Tally {
    /// An evaluation with `verdict`; `evaluated` is false for a later asynchronous result, which
    /// was counted when it started.
    fn of(verdict: &str, evaluated: bool) -> Tally {
        Tally {
            evaluations: i64::from(evaluated),
            hits: i64::from(matches!(verdict, "block" | "redact" | "flag" | "divert")),
            blocks: i64::from(verdict == "block"),
            redactions: i64::from(verdict == "redact"),
            flags: i64::from(verdict == "flag"),
            errors: i64::from(matches!(verdict, "error" | "error_open")),
        }
    }
}

#[derive(Debug, Default)]
struct Pending {
    events: VecDeque<Queued>,
    hours: HashMap<(String, i64), Counts>,
}

/// The node's side of safety events.
#[derive(Debug, Default)]
pub struct SafetyEvents {
    pending: Mutex<Pending>,
    announce: Mutex<VecDeque<Queued>>,
    /// Held while a batch is written, so a read that flushes waits for a write in progress.
    write: tokio::sync::Mutex<()>,
    dropped: AtomicU64,
}

impl SafetyEvents {
    fn clear(&self) {
        let mut p = self.pending.lock();
        p.events.clear();
        p.hours.clear();
        self.announce.lock().clear();
    }

    /// Events dropped because the queue was full.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Runtime {
    /// Queues the events and evaluation counts of a settled record (every entry), or of one entry
    /// an asynchronous guardrail completed afterwards (`only`, events and hits only).
    pub fn keep_safety(&self, record: &Value, marks: &[Mark], only: Option<usize>) {
        let Some(entries) = record.get("guardrails").and_then(Value::as_array) else {
            return;
        };
        let snap = self.gens.load();
        let now = self.now().0;
        let hour = now - now.rem_euclid(HOUR_US);
        let mut queued = Vec::new();
        let mut counts: Vec<(&str, Tally, Option<f64>)> = Vec::new();
        for (i, entry) in entries.iter().enumerate() {
            if only.is_some_and(|o| o != i) {
                continue;
            }
            let name = entry.get("name").and_then(Value::as_str).unwrap_or_default();
            let stage = entry.get("stage").and_then(Value::as_str).unwrap_or_default();
            let verdict = event::verdict_of(entry);
            let def = snap.guardrails.get(name);
            if stage != "route" && verdict != "skipped" {
                let ms = entry.get("duration_ms").and_then(Value::as_f64).filter(|_| {
                    only.is_none() && entry.get("verdict").and_then(Value::as_str) != Some("async")
                });
                counts.push((name, Tally::of(verdict, only.is_none()), ms));
            }
            if !event::is_finding(verdict) {
                continue;
            }
            let guardrail_type = if stage == "route" {
                "route_block".to_owned()
            } else {
                def.map_or("unknown", |d| d.type_name()).to_owned()
            };
            let mark = marks.iter().find(|m| m.entry == i);
            let ents = event::entities(entry);
            let category = entry.get("category").and_then(Value::as_str).unwrap_or("custom");
            let category = if category == "error" {
                event::type_category(&guardrail_type)
            } else {
                category
            };
            let input = event::fingerprint_input(name, category, mark.and_then(|m| m.digest.as_ref()), &ents);
            let verifier = self.pepper.verifier(&input);
            let digest = verifier.rsplit(':').next().unwrap_or_default();
            let facts = event::Facts {
                id: antirion_core::RequestId::generate().as_str().to_owned(),
                node_id: &self.node.id,
                guardrail_type,
                fingerprint: digest.get(..32).unwrap_or(digest).to_owned(),
            };
            if let Some(row) = event::row(record, entry, facts) {
                let notify = match def {
                    Some(d) => d.notify,
                    None => antirion_guardrails::def::severity_rank(&row.severity) >= 3,
                };
                queued.push(Queued {
                    row,
                    notify,
                    exposed: mark.map(|m| m.exposed.clone()).unwrap_or_default(),
                });
            }
        }
        if queued.is_empty() && counts.is_empty() {
            return;
        }
        let mut p = self.state.safety.pending.lock();
        for q in queued {
            if p.events.len() >= QUEUE_CAPACITY {
                p.events.pop_front();
                self.state.safety.dropped.fetch_add(1, Ordering::Relaxed);
            }
            p.events.push_back(q);
        }
        for (name, t, ms) in counts {
            let slot = p.hours.entry((name.to_owned(), hour)).or_default();
            slot.evaluations += t.evaluations;
            slot.hits += t.hits;
            slot.blocks += t.blocks;
            slot.redactions += t.redactions;
            slot.flags += t.flags;
            slot.errors += t.errors;
            if let Some(ms) = ms {
                slot.latency.add(ms);
            }
        }
    }

    /// Writes the queued events and counts; events stored are queued for their announcements.
    pub async fn flush_safety(&self) -> Result<(), StoreError> {
        let safety = &self.state.safety;
        let _writing = safety.write.lock().await;
        loop {
            let (batch, hours) = {
                let mut p = safety.pending.lock();
                let n = p.events.len().min(BATCH);
                let batch: Vec<Queued> = p.events.drain(..n).collect();
                (batch, std::mem::take(&mut p.hours))
            };
            if batch.is_empty() && hours.is_empty() {
                return Ok(());
            }
            let more = batch.len() == BATCH;
            let rows: Vec<SafetyEventRow> = batch.iter().map(|q| q.row.clone()).collect();
            if let Err(e) = self.store.insert_safety_events(rows).await {
                let mut p = safety.pending.lock();
                for q in batch.into_iter().rev() {
                    p.events.push_front(q);
                }
                restore(&mut p.hours, hours);
                return Err(e);
            }
            let encoded: Vec<GuardrailHour> = hours
                .iter()
                .map(|((g, h), c)| GuardrailHour {
                    guardrail: g.clone(),
                    hour_us: *h,
                    node_id: self.node.id.clone(),
                    evaluations: c.evaluations,
                    hits: c.hits,
                    blocks: c.blocks,
                    redactions: c.redactions,
                    flags: c.flags,
                    errors: c.errors,
                    latency: c.latency.encode(),
                })
                .collect();
            if let Err(e) = self
                .store
                .merge_guardrail_hours(encoded, crate::reqlog::rollup::merge_encoded)
                .await
            {
                restore(&mut safety.pending.lock().hours, hours);
                return Err(e);
            }
            {
                let mut a = safety.announce.lock();
                for q in batch.into_iter().filter(|q| q.notify || !q.exposed.is_empty()) {
                    if a.len() >= ANNOUNCE_CAPACITY {
                        a.pop_front();
                    }
                    a.push_back(q);
                }
            }
            if !more {
                return Ok(());
            }
        }
    }

    /// Sends what stored events ask for: `guardrail.event` at or above the guardrail's
    /// `notify.min_severity`, a `safety` alert instance for a critical one, and an `exposed` flag
    /// on each key found in the text.
    pub async fn announce_safety(&self) {
        let due: Vec<Queued> = self.state.safety.announce.lock().drain(..).collect();
        if due.is_empty() {
            return;
        }
        let snap = self.gens.load();
        for q in due {
            let e = &q.row;
            if q.notify {
                self.send_alert(&snap, None, "guardrail.event", event::announced(e));
                if e.severity == "critical" {
                    let scope = [
                        ("organization", e.organization.as_deref()),
                        ("team", e.team.as_deref()),
                        ("project", e.project.as_deref()),
                        ("guardrail", Some(e.guardrail.as_str())),
                        ("fingerprint", Some(e.fingerprint.as_str())),
                    ]
                    .into_iter()
                    .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned())))
                    .collect();
                    let summary = format!("Critical safety event: {} found {}", e.guardrail, e.category);
                    crate::alerting::raise_builtin(self, ALERT_RULE, "safety", "critical", scope, summary)
                        .await;
                }
            }
            for kid in &q.exposed {
                self.flag_exposed(&snap, kid, e).await;
            }
        }
    }

    /// Opens an `exposed` flag on the key whose id is `kid`.
    async fn flag_exposed(&self, snap: &crate::Snapshot, kid: &str, e: &SafetyEventRow) {
        let Some(key) = snap
            .identity
            .keys_by_kid
            .get(kid)
            .map(|s| std::sync::Arc::clone(&s.key))
        else {
            return;
        };
        let detail = serde_json::json!({"event": e.id, "log_id": e.log_id, "guardrail": e.guardrail});
        let finding = crate::people::anomalies::Finding {
            uid: key.uid,
            key: &key.name,
            kind: "exposed",
            subject: kid,
            count: 1,
            at_us: self.now().0,
            detail,
        };
        self.raise_flag(snap, finding).await;
        tracing::debug!(actor = ACTOR, key = %key.name, "a gateway key was found in content");
    }

    /// Deletes events and counts older than `before_us`.
    pub(crate) async fn expire_safety(&self, before_us: i64) -> Result<u64, StoreError> {
        self.store.expire_safety(before_us, None).await
    }

    /// Deletes the events of the end users an erasure names, within its scopes.
    pub(crate) async fn erase_safety_of(
        &self,
        f: &antirion_store::logs::LogFilter,
    ) -> Result<(), StoreError> {
        let users: Vec<String> = f
            .any_of
            .iter()
            .filter(|(c, _)| *c == antirion_store::logs::LogColumn::EndUser)
            .flat_map(|(_, vs)| vs.iter())
            .filter_map(|a| match a {
                antirion_store::logs::Arg::Text(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        if users.is_empty() {
            return Ok(());
        }
        self.read_safety().await;
        let sf = antirion_store::SafetyFilter {
            any_of: vec![(antirion_store::SafetyColumn::EndUser, users)],
            scopes: f.scopes.clone(),
            ..antirion_store::SafetyFilter::default()
        };
        self.store.expire_safety(0, Some(&sf)).await.map(|_| ())
    }

    /// Forgets every event and count, queued and stored.
    pub(crate) async fn clear_safety(&self) -> Result<(), StoreError> {
        let _writing = self.state.safety.write.lock().await;
        self.state.safety.clear();
        self.store.clear_safety().await
    }

    /// Writes what is queued before a read, and applies retention when it is due.
    pub async fn read_safety(&self) {
        self.read_logs().await;
        if let Err(e) = self.flush_safety().await {
            tracing::debug!(error = %e, "queued safety events could not be written before a read");
        }
    }
}

fn restore(into: &mut HashMap<(String, i64), Counts>, back: HashMap<(String, i64), Counts>) {
    for (k, c) in back {
        let slot = into.entry(k).or_default();
        slot.evaluations += c.evaluations;
        slot.hits += c.hits;
        slot.blocks += c.blocks;
        slot.redactions += c.redactions;
        slot.flags += c.flags;
        slot.errors += c.errors;
        slot.latency.merge(&c.latency);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tally_counts_its_verdict() {
        assert_eq!(
            Tally::of("redact", true),
            Tally {
                evaluations: 1,
                hits: 1,
                redactions: 1,
                ..Tally::default()
            }
        );
        assert_eq!(Tally::of("error_open", true).errors, 1);
        assert_eq!(Tally::of("pass", true).hits, 0);
        let later = Tally::of("flag", false);
        assert_eq!((later.evaluations, later.flags), (0, 1));
    }
}

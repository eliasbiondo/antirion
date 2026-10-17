//! The copy of a streamed response taken alongside the relay: the bytes the client was sent, kept
//! up to a bound, and written as an entry once the stream completed with its upstream terminator.

use std::sync::Arc;

use antirion_cache::{Entry, StoredEvent};
use antirion_runtime::Runtime;
use antirion_wire::sse::SseParser;
use bytes::Bytes;
use http::HeaderMap;
use serde_json::Value;

use super::CacheSlot;
use super::assemble::{assemble, usage};
use super::store::{Identity, attempt_storable, body_storable, guard_flagged, skipped, write};
use crate::outcome::Attempt;
use crate::reqlog::Draft;

/// Framing a client stream adds around its data payloads, at most, per byte of payload; bounds what
/// the tee keeps before it knows the payload sizes.
const FRAMING_ALLOWANCE: u64 = 2;

#[derive(Clone, Debug)]
pub(crate) struct Tee {
    slot: CacheSlot,
    identity: Identity,
    endpoint: &'static str,
    chunks: Vec<Bytes>,
    kept: u64,
    over: bool,
    terminated: bool,
}

impl Tee {
    /// A tee for the attempt's stream, when its response may be stored.
    pub(crate) fn new(a: &Attempt<'_>, status: u16, upstream: &HeaderMap) -> Option<Box<Tee>> {
        let slot = a.plan.cache.as_ref()?;
        attempt_storable(a, slot, status, upstream).then(|| {
            Box::new(Tee {
                slot: slot.clone(),
                identity: Identity::of(a, status),
                endpoint: a.plan.endpoint.id,
                chunks: Vec::new(),
                kept: 0,
                over: false,
                terminated: false,
            })
        })
    }

    /// Copies bytes sent to the client; past the bound the copy stops, the client stream does not.
    pub(crate) fn push(&mut self, bytes: &Bytes) {
        if self.over {
            return;
        }
        self.kept = self
            .kept
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        let bound = self
            .slot
            .max_entry_size
            .saturating_mul(FRAMING_ALLOWANCE)
            .saturating_add(64 << 10);
        if self.kept > bound {
            self.over = true;
            self.chunks.clear();
        } else {
            self.chunks.push(bytes.clone());
        }
    }

    /// The upstream sent its dialect's terminal event (or a complete response).
    pub(crate) fn terminated(&mut self) {
        self.terminated = true;
    }

    /// The stored events: data payloads in order, without the terminator and heartbeats.
    fn events(&self) -> Option<Vec<StoredEvent>> {
        let mut parser = SseParser::new(usize::MAX);
        let mut parsed = Vec::new();
        for c in &self.chunks {
            parser.push(c, &mut parsed).ok()?;
        }
        if let Ok(Some(last)) = parser.finish() {
            parsed.push(last);
        }
        Some(
            parsed
                .into_iter()
                .filter(|e| {
                    !e.data.is_empty() && e.data.trim() != "[DONE]" && e.event.as_deref() != Some("ping")
                })
                .map(|e| StoredEvent {
                    event: e.event,
                    data: e.data,
                })
                .collect(),
        )
    }

    /// Writes the entry of a stream that completed, when it is storable.
    pub(crate) async fn finish(self, rt: &Arc<Runtime>, log: &Draft, reported_usage: Option<&Value>) {
        let slot = self.slot.clone();
        match self.entry(rt, log, reported_usage) {
            Some(entry) => write(rt, &slot, entry).await,
            None => skipped(rt),
        }
    }

    fn entry(self, rt: &Runtime, log: &Draft, reported_usage: Option<&Value>) -> Option<Entry> {
        if self.over || !self.terminated || guard_flagged(log) {
            return None;
        }
        let events = self.events()?;
        let payload: usize = events.iter().map(|e| e.data.len()).sum();
        if u64::try_from(payload).unwrap_or(u64::MAX) > self.slot.max_entry_size || events.is_empty() {
            return None;
        }
        let assembled = assemble(self.endpoint, &events).ok()?;
        if assembled.as_ref().is_some_and(|v| !body_storable(v)) {
            return None;
        }
        let now_ms = rt.now().0 / 1000;
        let usage = usage(assembled.as_ref(), reported_usage);
        let body = assembled.and_then(|v| serde_json::to_string(&v).ok());
        Some(self.identity.entry(&self.slot, now_ms, body, Some(events), usage))
    }
}

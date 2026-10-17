//! `stream_chunk` hooks. Every event the client receives, after the output guardrails, goes through
//! the request's in-process extensions (built-in and Wasm; webhooks cannot run this hook), in the
//! reverse of the request hooks' order. A `mutate` patches the event's JSON payload; a `deny`, or a
//! failure under `fail_closed`, stops the stream: before the commit as the request's error response,
//! after it with the dialect's in-stream error event. The log gets one entry per extension that sums
//! its calls; the metrics count each call.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use antirion_error::GatewayError;
use antirion_ext::api::{Event, Input};
use antirion_ext::{Hook, HookAction, HookFacts, Plugin};
use antirion_obs::metrics::Fam;
use antirion_runtime::Runtime;
use antirion_runtime::extstats::CallEnd;
use antirion_wire::sse::{SseEvent, SseParser};
use bytes::Bytes;
use serde_json::{Value, json};

use super::ClientDialect;
use super::render;
use crate::extend::carrier::{self, Failure};
use crate::reqlog::Draft;
use crate::scrub::Scrub;

/// How a stream's events were stopped.
#[derive(Debug)]
pub(crate) struct Stopped {
    pub(crate) error: Box<GatewayError>,
    /// The stream end reason.
    pub(crate) reason: &'static str,
}

#[derive(Debug, Default, Clone)]
struct Tally {
    calls: u64,
    took: Duration,
    /// The last action other than `continue`.
    action: Option<&'static str>,
    failed_open: bool,
}

#[derive(Debug, Default)]
struct State {
    index: u64,
    tallies: Vec<Tally>,
    recorded: bool,
}

/// The `stream_chunk` hooks of one streamed request.
pub(crate) struct ChunkHooks {
    list: Vec<Arc<Plugin>>,
    facts: HookFacts,
    client: ClientDialect,
    rt: Arc<Runtime>,
    log: Draft,
    scrub: Scrub,
    state: Mutex<State>,
}

impl std::fmt::Debug for ChunkHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkHooks")
            .field("extensions", &self.list.len())
            .finish_non_exhaustive()
    }
}

impl ChunkHooks {
    /// The request's `stream_chunk` hooks; `None` when no attached extension has one.
    pub(crate) fn new(
        hooks: &crate::extend::Hooks,
        client: ClientDialect,
        rt: &Arc<Runtime>,
        log: &Draft,
        scrub: &Scrub,
    ) -> Option<Arc<ChunkHooks>> {
        let list: Vec<Arc<Plugin>> = hooks
            .list
            .iter()
            .rev()
            .filter(|p| p.runs(Hook::StreamChunk))
            .cloned()
            .collect();
        if list.is_empty() || client == ClientDialect::Bedrock {
            return None;
        }
        let tallies = vec![Tally::default(); list.len()];
        Some(Arc::new(ChunkHooks {
            list,
            facts: hooks.facts.clone(),
            client,
            rt: Arc::clone(rt),
            log: log.clone(),
            scrub: scrub.clone(),
            state: Mutex::new(State {
                tallies,
                ..State::default()
            }),
        }))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs the hooks over the events of `frames`; frames without a JSON event pass unchanged.
    pub(crate) async fn apply(&self, frames: Vec<Bytes>) -> Result<Vec<Bytes>, Stopped> {
        let mut out = Vec::with_capacity(frames.len());
        for frame in frames {
            let mut parser = SseParser::new(usize::MAX);
            let mut events: Vec<SseEvent> = Vec::new();
            let parsed = parser.push(&frame, &mut events).is_ok() && parser.buffered() == 0;
            if !parsed || events.is_empty() {
                out.push(frame);
                continue;
            }
            let mut rebuilt: Vec<Bytes> = Vec::with_capacity(events.len());
            let mut changed = false;
            for ev in events {
                let Ok(mut data) = serde_json::from_str::<Value>(&ev.data) else {
                    rebuilt.push(ev.raw);
                    continue;
                };
                if self.event(ev.event.as_deref(), &mut data).await? {
                    changed = true;
                    rebuilt.push(self.render(ev.event.as_deref(), &data));
                } else {
                    rebuilt.push(ev.raw);
                }
            }
            if changed {
                out.extend(rebuilt);
            } else {
                out.push(frame);
            }
        }
        Ok(out)
    }

    fn render(&self, name: Option<&str>, data: &Value) -> Bytes {
        match self.client {
            ClientDialect::Gemini => render::gemini_frame(data),
            _ => render::frame(name, &serde_json::to_vec(data).unwrap_or_default()),
        }
    }

    /// One event through every hook; true when a hook changed it.
    async fn event(&self, name: Option<&str>, data: &mut Value) -> Result<bool, Stopped> {
        let index = {
            let mut s = self.lock();
            s.index += 1;
            s.index
        };
        let mut changed = false;
        for (i, p) in self.list.iter().enumerate() {
            let input = Input::new(Hook::StreamChunk, &p.name, &p.config, &self.facts)
                .with_event(Event::new(index, name, data));
            let started = Instant::now();
            let result = carrier::in_process(p, &input)
                .await
                .unwrap_or_else(|| Err(Failure::Failed("a webhook cannot run this hook".into())));
            let took = started.elapsed();
            let outcome = match result {
                Ok(HookAction::Mutate { patch, metadata, .. }) => {
                    if !metadata.is_empty() {
                        self.log.update(|r| r.metadata.extend(metadata));
                    }
                    match antirion_ext::patch::apply(data, &patch) {
                        Ok(patched) => {
                            *data = patched;
                            changed |= !patch.is_empty();
                            Ok("mutate")
                        }
                        Err(e) => Err(Failure::Failed(format!("the patch did not apply: {e}"))),
                    }
                }
                Ok(HookAction::Deny { status, message }) => {
                    self.count(i, p, took, Some("deny"), None);
                    let error = crate::extend::denied(Hook::StreamChunk, status, &self.scrub.apply(&message));
                    self.finish();
                    return Err(Stopped {
                        error: Box::new(error),
                        reason: "extension_denied",
                    });
                }
                Ok(_) => Ok("continue"),
                Err(f) => Err(f),
            };
            match outcome {
                Ok(action) => self.count(i, p, took, Some(action), None),
                Err(f) => {
                    tracing::warn!(extension = %p.name, hook = "stream_chunk", error = %f.text(), "extension call failed");
                    self.count(i, p, took, None, Some(&f));
                    if !p.fail_open {
                        self.finish();
                        return Err(Stopped {
                            error: Box::new(crate::extend::unavailable(Hook::StreamChunk)),
                            reason: "extension_unavailable",
                        });
                    }
                }
            }
        }
        Ok(changed)
    }

    fn count(
        &self,
        i: usize,
        p: &Plugin,
        took: Duration,
        action: Option<&'static str>,
        failed: Option<&Failure>,
    ) {
        {
            let mut s = self.lock();
            if let Some(t) = s.tallies.get_mut(i) {
                t.calls += 1;
                t.took += took;
                if let Some(a) = action.filter(|a| *a != "continue") {
                    t.action = Some(a);
                }
                if failed.is_some() && p.fail_open {
                    t.failed_open = true;
                }
            }
        }
        let result = failed.map_or(action.unwrap_or("continue"), Failure::result);
        self.rt
            .metrics
            .add(Fam::ExtensionCalls, &[&p.name, "stream_chunk", result], 1.0);
        self.rt.metrics.observe(
            Fam::ExtensionDuration,
            &[&p.name, "stream_chunk"],
            took.as_secs_f64(),
        );
        let end = match failed {
            None => CallEnd::Ok,
            Some(Failure::Timeout) => CallEnd::TimedOut,
            Some(Failure::Failed(_)) => CallEnd::Failed,
        };
        let error = failed.map(Failure::text);
        self.rt
            .state
            .extensions
            .record(&p.name, took.as_secs_f64() * 1000.0, end, error.as_deref());
    }

    /// Writes one log entry per extension that ran, once.
    pub(crate) fn finish(&self) {
        let tallies = {
            let mut s = self.lock();
            if s.recorded {
                return;
            }
            s.recorded = true;
            s.tallies.clone()
        };
        self.log.update(|r| {
            for (p, t) in self.list.iter().zip(tallies).filter(|(_, t)| t.calls > 0) {
                r.extensions.push(json!({
                    "name": p.name,
                    "hook": "stream_chunk",
                    "action": t.action.unwrap_or("continue"),
                    "calls": t.calls,
                    "duration_ms": antirion_obs::timing::ms3(t.took.as_secs_f64() * 1000.0),
                    "failed_open": t.failed_open,
                }));
                if t.failed_open && !r.degraded.iter().any(|(c, _)| *c == "extensions") {
                    r.degraded.push(("extensions", Some("extension_error")));
                }
            }
        });
    }
}

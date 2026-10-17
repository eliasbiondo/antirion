//! The trace context of one request: the trace it belongs to (the client's, or a new one), its root
//! span, whether it is sampled, and what goes upstream as `traceparent` and `tracestate`.

use std::time::Instant;

use antirion_core::Clock;

use antirion_obs::settings::TraceSettings;
use antirion_obs::trace::{SpanData, SpanId, SpanKind, TraceId, format_traceparent, parse_traceparent};
use http::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

pub(crate) const TRACEPARENT: &str = "traceparent";
pub(crate) const TRACESTATE: &str = "tracestate";

/// The trace facts of one request.
#[derive(Clone, Debug)]
pub(crate) struct TraceCx {
    pub(crate) trace_id: TraceId,
    pub(crate) root: SpanId,
    /// The client's span, when it sent a valid `traceparent`.
    pub(crate) parent: Option<SpanId>,
    /// Spans are recorded and exported.
    pub(crate) sampled: bool,
    /// The sampled flag of the client's `traceparent`.
    parent_sampled: Option<bool>,
    tracestate: Option<HeaderValue>,
    propagate: bool,
    started: Instant,
    start_unix_ns: u64,
}

impl TraceCx {
    pub(crate) fn new(settings: &TraceSettings, headers: &HeaderMap, started: Instant) -> TraceCx {
        let parent = headers
            .get(TRACEPARENT)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_traceparent);
        let trace_id = parent.map_or_else(TraceId::random, |p| p.trace_id);
        let sampled = settings.enabled
            && settings.endpoint.is_some()
            && settings.sampler.sample(parent.map(|p| p.sampled), &trace_id);
        let start_unix_ns = u64::try_from(antirion_core::SystemClock.now().0)
            .unwrap_or_default()
            .saturating_mul(1000)
            .saturating_sub(u64::try_from(started.elapsed().as_nanos()).unwrap_or_default());
        TraceCx {
            trace_id,
            root: SpanId::random(),
            parent: parent.map(|p| p.span_id),
            sampled,
            parent_sampled: parent.map(|p| p.sampled),
            tracestate: parent.and_then(|_| headers.get(TRACESTATE).cloned()),
            propagate: settings.propagate_to_upstream,
            started,
            start_unix_ns,
        }
    }

    /// Decides sampling again under the request's `policies.observability.traces`: `enabled: false`
    /// turns spans off, and `sample_ratio` replaces the gateway sampler's ratio.
    pub(crate) fn scope(&mut self, settings: &TraceSettings, traces: Option<&Value>) {
        let enabled = traces
            .and_then(|t| t.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let sampler = match traces.and_then(|t| t.get("sample_ratio")).and_then(Value::as_f64) {
            Some(r) => settings.sampler.with_ratio(r),
            None => settings.sampler,
        };
        self.sampled = enabled
            && settings.enabled
            && settings.endpoint.is_some()
            && sampler.sample(self.parent_sampled, &self.trace_id);
    }

    /// The wall-clock time of `at`, in nanoseconds since the Unix epoch.
    pub(crate) fn unix_ns(&self, at: Instant) -> u64 {
        let offset = at.saturating_duration_since(self.started).as_nanos();
        self.start_unix_ns
            .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX))
    }

    /// A span of this request from `start` to `end`, child of the root.
    pub(crate) fn child(&self, name: String, kind: SpanKind, start: Instant, end: Instant) -> SpanData {
        let mut s = SpanData::new(self.trace_id, Some(self.root), name, kind);
        s.start_unix_nanos = self.unix_ns(start);
        s.end_unix_nanos = self.unix_ns(end);
        s
    }

    /// Replaces the client's trace headers on an upstream request with this request's context, the
    /// attempt span `span` as parent; removes them when propagation is off.
    pub(crate) fn propagate(&self, upstream: &mut HeaderMap, span: &SpanId) {
        upstream.remove(TRACEPARENT);
        upstream.remove(TRACESTATE);
        if !self.propagate {
            return;
        }
        let parent = if self.sampled { span } else { &self.root };
        if let Ok(v) = HeaderValue::from_str(&format_traceparent(&self.trace_id, parent, self.sampled)) {
            upstream.insert(HeaderName::from_static(TRACEPARENT), v);
        }
        if let Some(state) = &self.tracestate {
            upstream.insert(HeaderName::from_static(TRACESTATE), state.clone());
        }
    }
}

/// Applies the trace policy of the request's levels to its context and its record.
pub(crate) fn scope(cx: &mut crate::ctx::Ctx, levels: &crate::policy::Levels<'_>) {
    let obs = levels.effective("observability");
    let snap = std::sync::Arc::clone(&cx.snap);
    cx.trace.scope(&snap.obs.traces, obs.get("traces"));
    let t = cx.trace.clone();
    cx.log.update(|r| r.trace = t);
}

#[cfg(test)]
mod tests {
    use antirion_obs::ObsSettings;

    use super::*;

    #[test]
    fn a_valid_parent_is_continued_and_an_invalid_one_ignored() {
        let mut s = ObsSettings::default().traces;
        s.enabled = true;
        s.endpoint = Some("http://collector".into());
        let mut h = HeaderMap::new();
        h.insert(
            TRACEPARENT,
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00"),
        );
        h.insert(TRACESTATE, HeaderValue::from_static("a=b"));
        let t = TraceCx::new(&s, &h, Instant::now());
        assert_eq!(t.trace_id.hex(), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert!(!t.sampled);
        let mut up = HeaderMap::new();
        t.propagate(&mut up, &SpanId([7; 8]));
        let tp = up.get(TRACEPARENT).unwrap().to_str().unwrap();
        assert!(tp.starts_with("00-4bf92f3577b34da6a3ce929d0e0e4736-") && tp.ends_with("-00"));
        assert_eq!(up.get(TRACESTATE).unwrap(), "a=b");

        h.insert(TRACEPARENT, HeaderValue::from_static("garbage"));
        let fresh = TraceCx::new(&s, &h, Instant::now());
        assert_ne!(fresh.trace_id.hex(), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert!(fresh.sampled);
        let mut up = HeaderMap::new();
        fresh.propagate(&mut up, &SpanId([7; 8]));
        assert!(up.get(TRACESTATE).is_none());
        assert!(
            up.get(TRACEPARENT)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("-0707070707070707-01")
        );
    }
}

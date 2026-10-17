//! Traces: W3C context, sampling, finished spans and their OTLP encodings.

pub mod context;
pub mod otlp;

pub use self::context::{Parent, SpanId, TraceId, format_traceparent, parse_traceparent};

/// A span attribute value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    F64(f64),
    Bool(bool),
    StrArray(Vec<String>),
}

impl From<&str> for Value {
    fn from(s: &str) -> Value {
        Value::Str(s.to_owned())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Value {
        Value::Str(s)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Value {
        Value::Int(n)
    }
}

impl From<u64> for Value {
    fn from(n: u64) -> Value {
        Value::Int(i64::try_from(n).unwrap_or(i64::MAX))
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Value {
        Value::F64(n)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Value {
        Value::Bool(b)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SpanKind {
    Internal,
    Server,
    Client,
}

/// A span's status.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    #[default]
    Unset,
    Error(String),
}

/// A finished span.
#[derive(Clone, Debug, PartialEq)]
pub struct SpanData {
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub parent: Option<SpanId>,
    pub name: String,
    pub kind: SpanKind,
    pub start_unix_nanos: u64,
    pub end_unix_nanos: u64,
    pub attributes: Vec<(String, Value)>,
    pub status: Status,
}

impl SpanData {
    #[must_use]
    pub fn new(trace_id: TraceId, parent: Option<SpanId>, name: String, kind: SpanKind) -> SpanData {
        SpanData {
            trace_id,
            span_id: SpanId::random(),
            parent,
            name,
            kind,
            start_unix_nanos: 0,
            end_unix_nanos: 0,
            attributes: Vec::new(),
            status: Status::Unset,
        }
    }

    /// Sets an attribute, replacing an earlier value of the same key.
    pub fn set(&mut self, key: &str, value: impl Into<Value>) {
        let value = value.into();
        match self.attributes.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.attributes.push((key.to_owned(), value)),
        }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.attributes.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

/// How requests are chosen for tracing.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Sampler {
    ParentBasedRatio(f64),
    Ratio(f64),
    AlwaysOn,
    AlwaysOff,
}

impl Sampler {
    /// The same sampler with `ratio` in place of its own; samplers without a ratio are unchanged.
    #[must_use]
    pub fn with_ratio(&self, ratio: f64) -> Sampler {
        match *self {
            Sampler::Ratio(_) => Sampler::Ratio(ratio),
            Sampler::ParentBasedRatio(_) => Sampler::ParentBasedRatio(ratio),
            other => other,
        }
    }

    /// Whether a request with this parent (its sampled flag, if one was received) is traced.
    #[must_use]
    pub fn sample(&self, parent: Option<bool>, trace: &TraceId) -> bool {
        match *self {
            Sampler::AlwaysOn => true,
            Sampler::AlwaysOff => false,
            Sampler::Ratio(r) => by_ratio(r, trace),
            Sampler::ParentBasedRatio(r) => parent.unwrap_or_else(|| by_ratio(r, trace)),
        }
    }
}

fn by_ratio(ratio: f64, trace: &TraceId) -> bool {
    if ratio >= 1.0 {
        return true;
    }
    if ratio <= 0.0 || !ratio.is_finite() {
        return false;
    }
    let bound = (ratio * (u64::MAX as f64)) as u64;
    trace.low_u64() < bound
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samplers() {
        let t = TraceId::random();
        assert!(Sampler::ParentBasedRatio(0.0).sample(Some(true), &t));
        assert!(!Sampler::ParentBasedRatio(1.0).sample(Some(false), &t));
        assert!(!Sampler::ParentBasedRatio(0.0).sample(None, &t));
        assert!(Sampler::ParentBasedRatio(1.0).sample(None, &t));
        assert!(Sampler::AlwaysOn.sample(Some(false), &t));
        assert!(!Sampler::AlwaysOff.sample(Some(true), &t));
        assert!(Sampler::Ratio(1.0).sample(Some(false), &t));
        assert!(!Sampler::Ratio(0.0).sample(Some(true), &t));
        let low = TraceId([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let high = TraceId([0xff; 16]);
        assert!(Sampler::Ratio(0.5).sample(None, &low));
        assert!(!Sampler::Ratio(0.5).sample(None, &high));
    }

    #[test]
    fn attributes_replace() {
        let mut s = SpanData::new(TraceId::random(), None, "x".into(), SpanKind::Server);
        s.set("a", 1i64);
        s.set("a", "b");
        assert_eq!(s.get("a"), Some(&Value::Str("b".into())));
        assert_eq!(s.attributes.len(), 1);
    }
}

//! Overhead and latency measures of one request, from the intervals its stages recorded. Every
//! instant is in milliseconds since `t_start`.

/// An interval `[start, end]` in milliseconds since the request started.
pub type Span = (f64, f64);

/// What a request recorded for its timing.
#[derive(Clone, Debug, Default)]
pub struct Intervals {
    /// `t_end − t_start`.
    pub total_ms: f64,
    /// One per upstream attempt.
    pub upstream: Vec<Span>,
    /// Deliberate waits: backoff, `Retry-After`, admission queueing, hedge delay.
    pub waits: Vec<Span>,
    /// Calls to operator-configured services other than the upstream.
    pub external: Vec<Span>,
    /// The serving attempt's start to its first content, for streams.
    pub upstream_ttft_ms: Option<f64>,
    pub stream: Option<StreamInstants>,
}

/// The instants of a streamed response.
#[derive(Clone, Debug, Default)]
pub struct StreamInstants {
    pub first_byte_sent_ms: Option<f64>,
    pub first_upstream_content_ms: Option<f64>,
    /// Σ over relayed chunks of (handed to the transport − read from upstream), excluding time the
    /// client's back-pressure blocked the write.
    pub relay_ms: f64,
    pub first_content_sent_ms: Option<f64>,
    pub last_content_sent_ms: Option<f64>,
    pub output_tokens: u64,
}

/// The `timing` member of a record (in milliseconds, up to 3 decimals).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timing {
    pub total_ms: f64,
    pub overhead_ms: f64,
    pub upstream_ms: f64,
    pub wait_ms: f64,
    pub external_ms: f64,
    pub ttft_ms: Option<f64>,
    pub upstream_ttft_ms: Option<f64>,
    pub tpot_ms: Option<f64>,
    pub ttfb_overhead_ms: Option<f64>,
    pub relay_ms: Option<f64>,
}

/// Rounds to 3 decimals, never below zero.
#[must_use]
pub fn ms3(v: f64) -> f64 {
    if !v.is_finite() || v <= 0.0 {
        return 0.0;
    }
    (v * 1000.0).round() / 1000.0
}

/// The sorted union of `spans`, as disjoint intervals.
#[must_use]
pub fn union(spans: &[Span]) -> Vec<Span> {
    let mut v: Vec<Span> = spans.iter().copied().filter(|(a, b)| b > a).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<Span> = Vec::with_capacity(v.len());
    for (a, b) in v {
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

fn length(u: &[Span]) -> f64 {
    u.iter().map(|(a, b)| b - a).sum()
}

/// `|∪a \ ∪b|`.
#[must_use]
pub fn minus(a: &[Span], b: &[Span]) -> f64 {
    let b = union(b);
    union(a)
        .iter()
        .map(|&(s, e)| {
            let covered: f64 = b.iter().map(|&(bs, be)| (e.min(be) - s.max(bs)).max(0.0)).sum();
            (e - s) - covered
        })
        .sum()
}

fn clip(u: &[Span], lo: f64, hi: f64) -> Vec<Span> {
    u.iter()
        .map(|&(a, b)| (a.max(lo), b.min(hi)))
        .filter(|(a, b)| b > a)
        .collect()
}

impl Intervals {
    /// The measures of the overhead formula.
    #[must_use]
    pub fn timing(&self) -> Timing {
        let upstream = union(&self.upstream);
        let upstream_ms = length(&upstream);
        let wait_ms = minus(&self.waits, &upstream);
        let mut covered = upstream.clone();
        covered.extend(union(&self.waits));
        let external_ms = minus(&self.external, &covered);
        let mut t = Timing {
            total_ms: ms3(self.total_ms),
            upstream_ms: ms3(upstream_ms),
            wait_ms: ms3(wait_ms),
            external_ms: ms3(external_ms),
            overhead_ms: ms3(self.total_ms - upstream_ms - wait_ms),
            ..Timing::default()
        };
        let Some(s) = &self.stream else {
            return t;
        };
        t.upstream_ttft_ms = self.upstream_ttft_ms.map(ms3);
        t.ttft_ms = s.first_content_sent_ms.map(ms3);
        if s.output_tokens >= 2
            && let (Some(first), Some(last)) = (s.first_content_sent_ms, s.last_content_sent_ms)
        {
            t.tpot_ms = Some(ms3((last - first) / (s.output_tokens - 1) as f64));
        }
        let first_byte = s.first_byte_sent_ms.unwrap_or(self.total_ms);
        let content = s.first_upstream_content_ms.unwrap_or(first_byte);
        let waits_before: Vec<Span> = clip(&union(&self.waits), 0.0, first_byte);
        let up_before = length(&clip(&upstream, 0.0, content));
        let ttfb = first_byte - up_before - minus(&waits_before, &upstream);
        let ttfb = ms3(ttfb);
        let relay = ms3(s.relay_ms);
        t.ttfb_overhead_ms = Some(ttfb);
        t.relay_ms = Some(relay);
        t.overhead_ms = ms3(ttfb + relay);
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unions_merge_overlaps() {
        assert_eq!(
            union(&[(5.0, 7.0), (0.0, 2.0), (1.0, 3.0)]),
            vec![(0.0, 3.0), (5.0, 7.0)]
        );
        assert!((minus(&[(0.0, 10.0)], &[(2.0, 4.0), (3.0, 5.0)]) - 7.0).abs() < 1e-9);
    }

    #[test]
    fn overhead_excludes_upstream_and_waits() {
        let i = Intervals {
            total_ms: 412.2,
            upstream: vec![(0.3, 101.5), (301.8, 411.8)],
            waits: vec![(101.6, 301.6)],
            ..Intervals::default()
        };
        let t = i.timing();
        assert!((t.upstream_ms - 211.2).abs() < 0.01, "{t:?}");
        assert!((t.wait_ms - 200.0).abs() < 0.01, "{t:?}");
        assert!(t.overhead_ms < 2.0, "{t:?}");
        assert_eq!(t.ttft_ms, None);
        assert_eq!(t.tpot_ms, None);
    }

    #[test]
    fn overlapping_attempts_count_once() {
        let i = Intervals {
            total_ms: 360.0,
            upstream: vec![(5.0, 305.0), (55.0, 355.0)],
            ..Intervals::default()
        };
        let t = i.timing();
        assert!((t.upstream_ms - 350.0).abs() < 1e-9);
        assert!((t.overhead_ms - 10.0).abs() < 1e-9);
    }

    #[test]
    fn streams_split_overhead_and_never_divide_by_zero() {
        let i = Intervals {
            total_ms: 1000.0,
            upstream: vec![(2.0, 999.0)],
            upstream_ttft_ms: Some(198.0),
            stream: Some(StreamInstants {
                first_byte_sent_ms: Some(201.0),
                first_upstream_content_ms: Some(200.0),
                relay_ms: 3.5,
                first_content_sent_ms: Some(201.0),
                last_content_sent_ms: Some(901.0),
                output_tokens: 1,
            }),
            ..Intervals::default()
        };
        let t = i.timing();
        assert_eq!(t.ttfb_overhead_ms, Some(3.0));
        assert_eq!(t.relay_ms, Some(3.5));
        assert_eq!(t.overhead_ms, 6.5);
        assert_eq!(t.tpot_ms, None);
        assert_eq!(t.ttft_ms, Some(201.0));
    }

    #[test]
    fn rounding() {
        assert_eq!(ms3(1.23456), 1.235);
        assert_eq!(ms3(-3.0), 0.0);
        assert_eq!(ms3(f64::NAN), 0.0);
    }
}

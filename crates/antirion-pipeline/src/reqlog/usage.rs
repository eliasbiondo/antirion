//! The usage of a request as the record reports it: the provider's counts normalised into the one
//! usage record, with which counts were reported at all.

pub(crate) use antirion_limits::usage::Convention;
use antirion_limits::usage::{Usage as Normalised, normalize};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

/// The usage member of a record or an attempt.
#[derive(Debug, Default, serde::Serialize)]
pub(crate) struct UsageJson {
    input: Option<u64>,
    input_cache_read: Option<u64>,
    input_cache_write: Option<u64>,
    input_cache_write_1h: Option<u64>,
    input_audio: Option<u64>,
    input_image: Option<u64>,
    input_video: Option<u64>,
    output: Option<u64>,
    output_reasoning: Option<u64>,
    output_audio: Option<u64>,
    output_image: Option<u64>,
    total: Option<u64>,
    units: Option<()>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<&'static str>,
}

/// Token counts of a request or an attempt, as reported by the upstream or estimated. `input` and
/// `output` are `None` until reported; the subsets come from the latest report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Usage {
    pub(crate) input: Option<u64>,
    pub(crate) output: Option<u64>,
    /// A reported total, which settlement splits when it is all that was reported.
    pub(crate) total: Option<u64>,
    pub(crate) estimated: bool,
    pub(crate) subsets: Normalised,
}

impl Usage {
    /// An OpenAI-convention usage object (Chat Completions, or Responses names).
    pub(crate) fn openai(u: &Value) -> Usage {
        let responses = u.get("input_tokens").is_some() && u.get("prompt_tokens").is_none();
        let conv = if responses {
            Convention::OpenAiResponses
        } else {
            Convention::OpenAiChat
        };
        Usage {
            input: u
                .get("prompt_tokens")
                .or_else(|| u.get("input_tokens"))
                .and_then(Value::as_u64),
            output: u
                .get("completion_tokens")
                .or_else(|| u.get("output_tokens"))
                .and_then(Value::as_u64),
            total: u.get("total_tokens").and_then(Value::as_u64),
            estimated: false,
            subsets: normalize(conv, u).map(|(n, _)| n).unwrap_or_default(),
        }
    }

    /// The same counts with the subsets of `u` read under `conv`.
    pub(crate) fn with_subsets(mut self, conv: Convention, u: &Value) -> Usage {
        self.subsets = normalize(conv, u).map(|(n, _)| n).unwrap_or_default();
        self
    }

    /// Merges a later report: every non-null count, and every non-zero subset, replaces the earlier
    /// one.
    pub(crate) fn merge(&mut self, later: Usage) {
        if later.input.is_some() {
            self.input = later.input;
        }
        if later.output.is_some() {
            self.output = later.output;
        }
        if later.total.is_some() {
            self.total = later.total;
        }
        let (s, l) = (&mut self.subsets, &later.subsets);
        for (mine, theirs) in [
            (&mut s.input_cache_read, l.input_cache_read),
            (&mut s.input_cache_write, l.input_cache_write),
            (&mut s.input_cache_write_1h, l.input_cache_write_1h),
            (&mut s.input_audio, l.input_audio),
            (&mut s.input_image, l.input_image),
            (&mut s.input_video, l.input_video),
            (&mut s.output_reasoning, l.output_reasoning),
            (&mut s.output_audio, l.output_audio),
            (&mut s.output_image, l.output_image),
        ] {
            if theirs != 0 {
                *mine = theirs;
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.input.is_none() && self.output.is_none() && self.total.is_none()
    }

    /// The normalised record, with `input` and `output` as reported.
    pub(crate) fn normalised(&self) -> Normalised {
        Normalised {
            input: self.input.unwrap_or(0),
            output: self.output.unwrap_or(0),
            ..self.subsets
        }
    }

    /// The record's token fields (`total` included); null fields when nothing was reported.
    #[cfg(test)]
    pub(crate) fn fields(&self) -> Value {
        serde_json::to_value(self.json(None)).unwrap_or_default()
    }

    /// The record's token fields, with the record's `source` when given.
    pub(crate) fn json(&self, source: Option<&'static str>) -> UsageJson {
        if self.is_empty() {
            return UsageJson {
                source,
                ..UsageJson::default()
            };
        }
        let n = self.normalised();
        UsageJson {
            input: Some(n.input),
            input_cache_read: Some(n.input_cache_read),
            input_cache_write: Some(n.input_cache_write),
            input_cache_write_1h: Some(n.input_cache_write_1h),
            input_audio: Some(n.input_audio),
            input_image: Some(n.input_image),
            input_video: Some(n.input_video),
            output: Some(n.output),
            output_reasoning: Some(n.output_reasoning),
            output_audio: Some(n.output_audio),
            output_image: Some(n.output_image),
            total: Some(n.input.saturating_add(n.output)),
            units: None,
            source,
        }
    }

    /// The record's token types with their counts: `input` and `output` always, subsets when
    /// reported.
    pub(crate) fn token_types(&self) -> Vec<(&'static str, u64)> {
        let n = self.normalised();
        let mut out = vec![("input", n.input), ("output", n.output)];
        for (name, v) in [
            ("input_cache_read", n.input_cache_read),
            ("input_cache_write", n.input_cache_write),
            ("input_cache_write_1h", n.input_cache_write_1h),
            ("input_audio", n.input_audio),
            ("input_image", n.input_image),
            ("input_video", n.input_video),
            ("output_reasoning", n.output_reasoning),
            ("output_audio", n.output_audio),
            ("output_image", n.output_image),
        ] {
            if v > 0 {
                out.push((name, v));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_reports_keep_their_subsets() {
        let u = Usage::openai(&json!({"prompt_tokens": 30, "completion_tokens": 12,
            "prompt_tokens_details": {"cached_tokens": 10}, "completion_tokens_details": {"reasoning_tokens": 5}}));
        let f = u.fields();
        assert_eq!(
            (f["input"].as_u64(), f["input_cache_read"].as_u64()),
            (Some(30), Some(10))
        );
        assert_eq!(
            (f["output"].as_u64(), f["output_reasoning"].as_u64()),
            (Some(12), Some(5))
        );
        assert_eq!(f["total"], 42);
        assert_eq!(
            u.token_types(),
            [
                ("input", 30),
                ("output", 12),
                ("input_cache_read", 10),
                ("output_reasoning", 5)
            ]
        );
    }

    #[test]
    fn later_reports_replace_earlier_counts() {
        let mut u = Usage::openai(&json!({"prompt_tokens": 3}));
        u.merge(Usage::openai(&json!({"completion_tokens": 5})));
        assert_eq!((u.input, u.output), (Some(3), Some(5)));
        assert!(Usage::default().fields()["total"].is_null());
    }
}

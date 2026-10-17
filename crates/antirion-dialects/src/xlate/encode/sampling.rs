//! Generation controls shared by the upstream codecs: sampling parameters, stop sequences, output
//! limits and structured output.

use serde_json::Value;

use super::Enc;
use crate::xlate::ir::{Choice, Format, Num};
use crate::xlate::target::Upstream;
use crate::xlate::warn::{Kind, Src};

/// How a target receives the request's structured-output setting.
pub(super) enum Structured {
    None,
    /// A JSON Schema (`None` for JSON-object mode) the target takes natively.
    Native(Option<Value>),
    /// A forced `json_response` tool with this schema.
    Emulated(Value),
}

impl Enc<'_> {
    /// Reports a field the target cannot take.
    pub(super) fn drop_field(&mut self, n: Option<&Num>) {
        if let Some(n) = n {
            self.warn(Kind::Dropped, &n.src);
        }
    }

    /// Whether Anthropic thinking constraints apply to a sampling field: dropped when thinking is on.
    fn thinking_drops(&mut self, n: &Num, thinking: bool, keep: impl Fn(f64) -> bool) -> bool {
        if thinking && !n.f64().is_some_and(keep) {
            self.warn(Kind::Dropped, &n.src);
            return true;
        }
        false
    }

    /// `temperature` for the target: clamped into its range, dropped when the model rejects
    /// sampling parameters or when thinking needs exactly 1.
    pub(super) fn temperature(&mut self, thinking: bool) -> Option<Value> {
        let n = self.req.temperature.clone()?;
        if !self.t.sampling_params {
            self.warn(Kind::Dropped, &n.src);
            return None;
        }
        if self.thinking_drops(&n, thinking, |t| (t - 1.0).abs() < f64::EPSILON) {
            return None;
        }
        let Some(v) = n.f64() else {
            return Some(n.value);
        };
        if let Some((lo, hi)) = self.t.temperature_range
            && (v < lo || v > hi)
        {
            self.warn(Kind::Clamped, &n.src);
            let c = v.clamp(lo, hi);
            return Some(if c.fract() == 0.0 && c.abs() < 1e15 {
                Value::from(c as i64)
            } else {
                Value::from(c)
            });
        }
        Some(n.value)
    }

    /// `top_p`, dropped where the model rejects it next to `temperature` or under thinking.
    pub(super) fn top_p(&mut self, thinking: bool, temperature_sent: bool) -> Option<Value> {
        let n = self.req.top_p.clone()?;
        if !self.t.sampling_params {
            self.warn(Kind::Dropped, &n.src);
            return None;
        }
        if self.thinking_drops(&n, thinking, |p| (0.95..=1.0).contains(&p)) {
            return None;
        }
        if self.t.exclusive_temperature_top_p && temperature_sent {
            self.warn(Kind::Dropped, &n.src);
            return None;
        }
        Some(n.value)
    }

    /// `top_k` for a target that has the field.
    pub(super) fn top_k(&mut self, thinking: bool) -> Option<Value> {
        let n = self.req.top_k.clone()?;
        if !self.t.sampling_params || thinking {
            self.warn(Kind::Dropped, &n.src);
            return None;
        }
        Some(n.value)
    }

    /// The stop sequences within the target's limit.
    pub(super) fn stop(&mut self) -> Option<Vec<String>> {
        let (list, src) = self.req.stop.clone()?;
        if self.dialect() == Upstream::Responses {
            self.warn(Kind::Dropped, &src);
            return None;
        }
        match self.t.stop_limit() {
            Some(limit) if list.len() > limit => {
                self.warn(Kind::Dropped, &src);
                Some(list.into_iter().take(limit).collect())
            }
            _ => Some(list),
        }
    }

    /// `max_tokens` for an Anthropic-dialect target: the client's value or the deployment default.
    pub(super) fn anthropic_max_tokens(&mut self) -> u64 {
        if let Some((v, _)) = &self.req.max_tokens {
            return *v;
        }
        self.warn(Kind::Defaulted, &Src::absent("max_tokens"));
        self.t.max_output_tokens.unwrap_or(self.t.default_max_tokens)
    }

    /// Structured output for the target; emulation is refused next to a forced client tool.
    pub(super) fn structured(&mut self, native: bool) -> Structured {
        let Some((format, src)) = self.req.format.clone() else {
            return Structured::None;
        };
        let schema = match &format {
            Format::JsonSchema { schema, .. } => Some(schema.clone()),
            Format::JsonObject => None,
        };
        if native {
            if schema.is_none() && self.dialect() == Upstream::Anthropic {
                self.warn(Kind::Approximated, &src);
                return Structured::Native(Some(serde_json::json!({"type": "object"})));
            }
            return Structured::Native(schema);
        }
        let forced = matches!(
            self.req.choice.as_ref().map(|c| &c.0),
            Some(Choice::Required | Choice::Named(_) | Choice::Allowed { required: true, .. })
        );
        if !self.req.tools.is_empty() && forced {
            self.refuse(
                &src,
                "Structured output would need a forced tool, and the request already forces one of its own.",
            );
            return Structured::None;
        }
        self.warn(Kind::Emulated, &src);
        self.json_tool = true;
        Structured::Emulated(schema.unwrap_or_else(|| serde_json::json!({"type": "object"})))
    }

    /// The schema name a Chat or Responses target receives.
    pub(super) fn format_name(&self) -> String {
        match self.req.format.as_ref().map(|f| &f.0) {
            Some(Format::JsonSchema { name: Some(n), .. }) => n.clone(),
            _ => "response".to_owned(),
        }
    }

    pub(super) fn format_strict(&self) -> Option<bool> {
        match self.req.format.as_ref().map(|f| &f.0) {
            Some(Format::JsonSchema { strict, .. }) => *strict,
            _ => None,
        }
    }
}

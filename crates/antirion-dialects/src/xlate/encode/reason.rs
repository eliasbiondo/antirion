//! Reasoning controls encoded for the target's `reasoning_control`.

use serde_json::{Map, Value};

use super::Enc;
use crate::xlate::ir::{Effort, Mode};
use crate::xlate::target::Control;
use crate::xlate::warn::Kind;

/// Effort by budget thresholds.
fn effort_of_budget(b: u64) -> Effort {
    if b < 4096 {
        Effort::Low
    } else if b < 16384 {
        Effort::Medium
    } else {
        Effort::High
    }
}

/// The nearest supported effort in `order`, ties to the higher one.
fn nearest(want: Effort, supported: &[Effort], order: &[Effort]) -> Option<Effort> {
    let rank = |e: Effort| order.iter().position(|x| *x == e);
    let target = rank(want)?;
    supported
        .iter()
        .filter_map(|e| rank(*e).map(|r| (*e, r)))
        .min_by_key(|(_, r)| (r.abs_diff(target), std::cmp::Reverse(*r)))
        .map(|(e, _)| e)
}

const EFFORT_ORDER: [Effort; 6] = [
    Effort::None,
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
];
const ADAPTIVE_ORDER: [Effort; 5] = [
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// What an Anthropic-family target receives.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Thinking {
    /// No `thinking` member.
    Omitted,
    Disabled,
    Enabled(u64),
    Adaptive,
}

impl Thinking {
    pub(super) fn on(&self) -> bool {
        matches!(self, Thinking::Enabled(_) | Thinking::Adaptive)
    }

    pub(super) fn value(&self, display: bool) -> Value {
        let mut m = Map::new();
        match self {
            Thinking::Omitted => return Value::Null,
            Thinking::Disabled => {
                m.insert("type".into(), "disabled".into());
            }
            Thinking::Enabled(b) => {
                m.insert("type".into(), "enabled".into());
                m.insert("budget_tokens".into(), (*b).into());
            }
            Thinking::Adaptive => {
                m.insert("type".into(), "adaptive".into());
            }
        }
        if display && self.on() {
            m.insert("display".into(), "summarized".into());
        }
        Value::Object(m)
    }
}

impl Enc<'_> {
    fn supported(&self) -> Vec<Effort> {
        self.t.efforts.iter().filter_map(|e| Effort::parse(e)).collect()
    }

    fn approximate(&mut self) {
        let src = self.req.reason.src.clone();
        self.warn(Kind::Approximated, &src);
    }

    fn drop_reasoning(&mut self) {
        let src = self.req.reason.src.clone();
        self.warn(Kind::Dropped, &src);
    }

    /// Whether the client asked for reasoning at all.
    pub(super) fn reasoning_asked(&self) -> bool {
        self.req.reason.mode != Mode::Unspecified
    }

    /// The effort an `effort`-controlled target (OpenAI) receives.
    pub(super) fn effort(&mut self) -> Option<String> {
        let r = self.req.reason.clone();
        match self.t.control {
            Control::Effort => {}
            Control::None => {
                if matches!(r.mode, Mode::On | Mode::Adaptive) {
                    self.drop_reasoning();
                }
                return None;
            }
            _ => {
                if matches!(r.mode, Mode::On | Mode::Adaptive) {
                    self.approximate();
                }
                return None;
            }
        }
        let supported = self.supported();
        match r.mode {
            Mode::Unspecified => None,
            Mode::Off => {
                if supported.contains(&Effort::None) {
                    Some("none".into())
                } else {
                    None
                }
            }
            Mode::On | Mode::Adaptive => {
                let (want, exact) = match (r.effort, r.budget) {
                    (Some(e), _) => (if e == Effort::Max { Effort::Xhigh } else { e }, e != Effort::Max),
                    (None, Some(b)) => (effort_of_budget(b), false),
                    (None, None) if r.mode == Mode::Adaptive => (Effort::High, false),
                    (None, None) => (Effort::Medium, false),
                };
                let got = nearest(want, &supported, &EFFORT_ORDER).unwrap_or(want);
                if !exact || got != want {
                    self.approximate();
                }
                Some(got.as_str().to_owned())
            }
        }
    }

    /// Thinking for an Anthropic-family target, given its `max_tokens`, and the effort for
    /// `output_config.effort`.
    pub(super) fn anthropic_thinking(&mut self, max_tokens: u64) -> (Thinking, Option<String>) {
        let r = self.req.reason.clone();
        match self.t.control {
            Control::Adaptive => self.adaptive(max_tokens),
            Control::Budget => match r.mode {
                Mode::Unspecified | Mode::Off => (Thinking::Omitted, None),
                Mode::On | Mode::Adaptive => {
                    let (want, exact) = match (r.budget, r.effort) {
                        (Some(b), _) => (b, true),
                        (None, Some(e)) => (self.budget_for(e), false),
                        (None, None) => (self.budget_for(Effort::High), false),
                    };
                    if max_tokens.saturating_sub(1) < 1024 {
                        self.drop_reasoning();
                        return (Thinking::Omitted, None);
                    }
                    let got = want.clamp(1024, max_tokens - 1);
                    if !exact || got != want {
                        self.approximate();
                    }
                    (Thinking::Enabled(got), None)
                }
            },
            Control::None => {
                if matches!(r.mode, Mode::On | Mode::Adaptive) {
                    self.drop_reasoning();
                }
                (Thinking::Omitted, None)
            }
            Control::Effort | Control::Level => {
                if matches!(r.mode, Mode::On | Mode::Adaptive) {
                    self.drop_reasoning();
                }
                (Thinking::Omitted, None)
            }
        }
    }

    fn adaptive(&mut self, max_tokens: u64) -> (Thinking, Option<String>) {
        let r = self.req.reason.clone();
        let supported = self.supported();
        let pick = |e: Effort| {
            let e = if e == Effort::Minimal { Effort::Low } else { e };
            nearest(e, &supported, &ADAPTIVE_ORDER).unwrap_or(e)
        };
        match r.mode {
            Mode::Unspecified => (Thinking::Omitted, None),
            Mode::Off => {
                if self.t.can_disable {
                    (Thinking::Disabled, None)
                } else {
                    self.approximate();
                    let lowest = ADAPTIVE_ORDER
                        .into_iter()
                        .find(|e| supported.contains(e))
                        .unwrap_or(Effort::Low);
                    (Thinking::Omitted, Some(lowest.as_str().to_owned()))
                }
            }
            Mode::On | Mode::Adaptive => {
                if let (Some(b), None) = (r.budget, r.effort) {
                    if self.t.budget_ok && max_tokens.saturating_sub(1) >= 1024 {
                        let got = b.clamp(1024, max_tokens - 1);
                        if got != b {
                            self.approximate();
                        }
                        return (Thinking::Enabled(got), None);
                    }
                    self.approximate();
                    let e = pick(effort_of_budget(b));
                    return (Thinking::Adaptive, Some(e.as_str().to_owned()));
                }
                let effort = r.effort.map(|e| {
                    let got = pick(e);
                    (got, got != e)
                });
                if effort.is_some_and(|(_, changed)| changed) {
                    self.approximate();
                }
                (Thinking::Adaptive, effort.map(|(e, _)| e.as_str().to_owned()))
            }
        }
    }

    fn budget_for(&self, e: Effort) -> u64 {
        let b = &self.policy.budgets.0;
        let i = match e {
            Effort::None | Effort::Minimal => 0,
            Effort::Low => 1,
            Effort::Medium => 2,
            Effort::High => 3,
            Effort::Xhigh => 4,
            Effort::Max => 5,
        };
        b.get(i).copied().unwrap_or(8192)
    }

    /// Gemini `thinkingConfig`.
    pub(super) fn gemini_thinking(&mut self) -> Option<Value> {
        let r = self.req.reason.clone();
        let mut m = Map::new();
        match self.t.control {
            Control::Budget => {
                let budget: i64 = match r.mode {
                    Mode::Unspecified => {
                        if r.include_thoughts != Some(true) {
                            return None;
                        }
                        m.insert("includeThoughts".into(), true.into());
                        return Some(Value::Object(m));
                    }
                    Mode::Off if self.t.can_disable => 0,
                    Mode::Off => {
                        self.approximate();
                        i64::try_from(self.t.min_budget.unwrap_or(0)).unwrap_or(0)
                    }
                    Mode::Adaptive if r.budget.is_none() && r.effort.is_none() => -1,
                    Mode::On | Mode::Adaptive => {
                        let (want, exact) = match (r.budget, r.effort) {
                            (Some(b), _) => (b, true),
                            (None, Some(e)) => (self.budget_for(e), false),
                            (None, None) => (self.budget_for(Effort::High), false),
                        };
                        let lo = self.t.min_budget.unwrap_or(0);
                        let hi = self.t.max_budget.unwrap_or(u64::MAX).max(lo);
                        let got = want.clamp(lo, hi);
                        if !exact || got != want {
                            self.approximate();
                        }
                        i64::try_from(got).unwrap_or(i64::MAX)
                    }
                };
                m.insert("thinkingBudget".into(), budget.into());
            }
            Control::Level => match r.mode {
                Mode::Unspecified | Mode::Off => {
                    if r.mode == Mode::Off {
                        self.approximate();
                    }
                }
                Mode::On | Mode::Adaptive => {
                    let (want, exact) = match (r.effort, r.budget) {
                        (Some(e), _) => (if e == Effort::Max { Effort::Xhigh } else { e }, e != Effort::Max),
                        (None, Some(b)) => (effort_of_budget(b), false),
                        (None, None) => (Effort::High, false),
                    };
                    let got = nearest(want, &self.supported(), &EFFORT_ORDER).unwrap_or(want);
                    if !exact || got != want {
                        self.approximate();
                    }
                    m.insert("thinkingLevel".into(), got.as_str().into());
                }
            },
            Control::None | Control::Effort | Control::Adaptive => {
                if matches!(r.mode, Mode::On | Mode::Adaptive) {
                    self.drop_reasoning();
                }
            }
        }
        if r.include_thoughts == Some(true) && !m.is_empty() {
            m.insert("includeThoughts".into(), true.into());
        }
        (!m.is_empty()).then_some(Value::Object(m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_ties_up() {
        let s = [Effort::Low, Effort::High];
        assert_eq!(nearest(Effort::Medium, &s, &EFFORT_ORDER), Some(Effort::High));
        assert_eq!(
            nearest(
                Effort::Xhigh,
                &[Effort::Low, Effort::Medium, Effort::High],
                &EFFORT_ORDER
            ),
            Some(Effort::High)
        );
        assert_eq!(
            nearest(Effort::Minimal, &[Effort::Low, Effort::Medium], &EFFORT_ORDER),
            Some(Effort::Low)
        );
        assert_eq!(effort_of_budget(5000), Effort::Medium);
    }
}

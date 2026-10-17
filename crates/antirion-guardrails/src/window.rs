//! Window mode for one segment stream: after each delta the window-capable checks scan the text so
//! far; a confirmed match of a blocking guardrail blocks, redactions are kept, and the release point
//! `|T| - holdback` is lowered so no span (nor a match that may still grow) is cut.

use std::sync::Arc;

use crate::def::{Action, Category, Def};
use crate::pattern::Span;
use crate::redact;
use crate::runner::spans_in;
use crate::tokens::Tokens;

/// One redaction: a byte range of the stream's text and what replaces it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redaction {
    pub start: usize,
    pub end: usize,
    pub with: String,
}

/// What one scan concluded.
#[derive(Clone, Debug)]
pub enum Step {
    /// Text may be released up to this byte offset; the redactions found so far.
    Release(usize, Vec<Redaction>),
    /// A blocking guardrail matched.
    Block {
        def: Arc<Def>,
        category: Category,
        detail: String,
    },
}

/// The window-capable enforcing output guardrails of a request.
#[derive(Debug, Clone)]
pub struct Window {
    defs: Vec<Arc<Def>>,
    holdback: usize,
    hash_key: Option<Vec<u8>>,
    /// Reversible tokens to put back, after every check.
    tokens: Option<Arc<Tokens>>,
}

/// The byte offset `n` code points before the end of `text` (0 when it is shorter).
fn back(text: &str, n: usize) -> usize {
    if n == 0 {
        return text.len();
    }
    text.char_indices().rev().nth(n - 1).map_or(0, |(i, _)| i)
}

impl Window {
    #[must_use]
    pub fn new(defs: Vec<Arc<Def>>, holdback: usize, hash_key: Option<Vec<u8>>) -> Window {
        Window {
            defs,
            holdback,
            hash_key,
            tokens: None,
        }
    }

    /// Also puts the request's reversible tokens back as the text is released.
    #[must_use]
    pub fn restoring(mut self, tokens: Arc<Tokens>) -> Window {
        self.tokens = Some(tokens);
        self
    }

    /// Scans `text` (everything received so far for one segment stream). `ended` once the upstream
    /// ended the stream: then nothing is tentative and everything may be released.
    #[must_use]
    pub fn step(&self, text: &str, ended: bool) -> Step {
        let mut redactions: Vec<Redaction> = Vec::new();
        let mut holds: Vec<(usize, usize)> = Vec::new();
        for def in &self.defs {
            let spans: Vec<Span> = redact::merge(spans_in(def, text));
            for s in spans {
                let tentative = !ended && s.end >= text.len();
                if tentative {
                    holds.push((s.start, s.end));
                    continue;
                }
                match def.action {
                    Action::Block => {
                        return Step::Block {
                            def: Arc::clone(def),
                            category: category_of(def),
                            detail: s.label.to_ascii_lowercase(),
                        };
                    }
                    Action::Redact => {
                        let matched = text.get(s.start..s.end).unwrap_or_default();
                        let with = redact::masked(
                            matched,
                            &s.label,
                            def.replacement,
                            self.hash_key.as_deref(),
                            def.keep(),
                        );
                        if !redactions.iter().any(|r| r.start < s.end && s.start < r.end) {
                            redactions.push(Redaction {
                                start: s.start,
                                end: s.end,
                                with,
                            });
                        }
                    }
                    Action::Flag | Action::Divert => {}
                }
            }
        }
        if let Some(tokens) = &self.tokens {
            for r in tokens.find(text) {
                if !redactions.iter().any(|x| x.start < r.end && r.start < x.end) {
                    redactions.push(r);
                }
            }
            if !ended && let Some(at) = tokens.pending(text) {
                holds.push((at, text.len()));
            }
        }
        redactions.sort_by_key(|r| r.start);
        let mut p = if ended {
            text.len()
        } else {
            back(text, self.holdback)
        };
        loop {
            let straddling = redactions
                .iter()
                .filter(|r| r.start < p && p < r.end)
                .map(|r| r.start);
            let growing = holds.iter().filter(|(s, _)| *s < p).map(|(s, _)| *s);
            let lowered = straddling.chain(growing).min();
            match lowered {
                Some(s) if s < p => p = s,
                _ => break,
            }
        }
        Step::Release(p, redactions)
    }
}

fn category_of(def: &Def) -> Category {
    match def.check {
        crate::def::Check::Keywords(_) => Category::Keyword,
        crate::def::Check::Pii(_) => Category::Pii,
        crate::def::Check::Secrets(_) => Category::Secret,
        _ => Category::DenyPattern,
    }
}

/// `text[from..to]` with the redactions inside the range applied.
#[must_use]
pub fn render(text: &str, redactions: &[Redaction], from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut at = from;
    for r in redactions.iter().filter(|r| r.start >= from && r.end <= to) {
        out.push_str(text.get(at..r.start).unwrap_or_default());
        out.push_str(&r.with);
        at = r.end;
    }
    out.push_str(text.get(at..to).unwrap_or_default());
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::compile::compile;

    fn window(def: serde_json::Value, holdback: usize) -> Window {
        let g = compile(&json!({"guardrails": {"definitions": {"g": def}}})).unwrap();
        Window::new(vec![Arc::clone(&g.defs["g"])], holdback, None)
    }

    /// Feeds `text` one character at a time and returns what was released.
    fn stream(w: &Window, text: &str) -> Result<Vec<String>, ()> {
        let mut t = String::new();
        let mut released = 0;
        let mut out = Vec::new();
        let chars: Vec<char> = text.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            t.push(*c);
            let ended = i + 1 == chars.len();
            match w.step(&t, ended) {
                Step::Block { .. } => return Err(()),
                Step::Release(p, r) => {
                    if p > released {
                        out.push(render(&t, &r, released, p));
                        released = p;
                    }
                }
            }
        }
        Ok(out)
    }

    #[test]
    fn a_phone_number_is_never_released_unredacted() {
        let w = window(json!({"type": "pii", "action": "redact"}), 256);
        let out = stream(&w, "call me at +44 20 7946 0958 please").unwrap();
        assert_eq!(out.concat(), "call me at [PHONE] please");
        assert!(out.iter().all(|p| !p.chars().any(|c| c.is_ascii_digit())));
    }

    #[test]
    fn tokens_split_across_deltas_are_restored_whole() {
        let tokens = Arc::new(Tokens::new(vec![("[EMAIL_1]".into(), "ana@acme.example".into())]));
        let w = Window::new(Vec::new(), 0, None).restoring(tokens);
        let out = stream(&w, "write to [EMAIL_1] today, [EMAIL_2] stays").unwrap();
        assert_eq!(out.concat(), "write to ana@acme.example today, [EMAIL_2] stays");
        assert!(out.iter().all(|p| !p.contains("[EMAIL_1")));
    }

    #[test]
    fn a_split_keyword_blocks_before_any_fragment_is_released() {
        let w = window(
            json!({"type": "keywords", "keywords": {"words": ["forbiddenword"]}}),
            16,
        );
        let text = format!("{}forbiddenword and more", "clean text ".repeat(5));
        assert!(stream(&w, &text).is_err());
        let w = window(
            json!({"type": "keywords", "keywords": {"words": ["forbidden"]}}),
            16,
        );
        let out = stream(&w, "clean words forbiddenness end").unwrap();
        assert_eq!(out.concat(), "clean words forbiddenness end");
    }
}

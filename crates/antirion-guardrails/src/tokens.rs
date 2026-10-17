//! Reversible tokenisation: the input stage replaces each detected value with a numbered token
//! (`[EMAIL_1]`), the same value always with the same token, and the output stage puts the values
//! back where the model repeated the tokens. The mapping lives only as long as the request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::pattern::Span;
use crate::segments::{Dialect, output_segments, write_back};
use crate::window::Redaction;

/// The mapping a request's input stage builds, one value at a time.
#[derive(Debug, Default)]
pub struct Tokenizer {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// `(label, value)` → token.
    by_value: HashMap<(String, String), String>,
    /// The next number per label.
    next: HashMap<String, u32>,
    pairs: Vec<(String, String)>,
    /// Every input text, so a token never repeats text the client wrote.
    seen: String,
}

impl Tokenizer {
    /// Records the client's texts: a token that already appears in them is never handed out.
    pub fn saw<'a>(&self, texts: impl Iterator<Item = &'a str>) {
        if let Ok(mut s) = self.state.lock() {
            for t in texts {
                s.seen.push_str(t);
                s.seen.push('\u{0}');
            }
        }
    }

    /// The token of `value`, an entity labelled `label`.
    pub fn token(&self, label: &str, value: &str) -> String {
        let Ok(mut s) = self.state.lock() else {
            return format!("[{}]", label.to_uppercase());
        };
        let key = (label.to_owned(), value.to_owned());
        if let Some(t) = s.by_value.get(&key) {
            return t.clone();
        }
        let upper = label.to_uppercase();
        let mut n = s.next.get(&upper).copied().unwrap_or(1);
        let token = loop {
            let t = format!("[{upper}_{n}]");
            n += 1;
            if !s.seen.contains(&t) {
                break t;
            }
        };
        s.next.insert(upper, n);
        s.by_value.insert(key, token.clone());
        s.pairs.push((token.clone(), value.to_owned()));
        token
    }

    /// `text` with every span replaced by its value's token.
    pub fn apply(&self, text: &str, spans: &[Span]) -> String {
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for s in spans {
            out.push_str(text.get(at..s.start).unwrap_or_default());
            out.push_str(&self.token(&s.label, text.get(s.start..s.end).unwrap_or_default()));
            at = s.end;
        }
        out.push_str(text.get(at..).unwrap_or_default());
        out
    }

    /// The finished mapping; `None` when nothing was tokenised.
    pub fn finish(&self) -> Option<Arc<Tokens>> {
        let pairs = self.state.lock().ok()?.pairs.clone();
        (!pairs.is_empty()).then(|| Arc::new(Tokens::new(pairs)))
    }
}

/// The tokens of one request and their values.
#[derive(Debug, Clone)]
pub struct Tokens {
    pairs: Vec<(String, String)>,
    longest: usize,
}

impl Tokens {
    #[must_use]
    pub fn new(pairs: Vec<(String, String)>) -> Tokens {
        let longest = pairs.iter().map(|(t, _)| t.len()).max().unwrap_or(0);
        Tokens { pairs, longest }
    }

    /// Every whole token in `text`, as byte ranges with the value that replaces them, in order.
    #[must_use]
    pub fn find(&self, text: &str) -> Vec<Redaction> {
        let mut out = Vec::new();
        let bytes = text.as_bytes();
        let mut i = 0;
        while let Some(off) = text.get(i..).and_then(|t| t.find('[')) {
            let start = i + off;
            let found = self
                .pairs
                .iter()
                .find(|(t, _)| bytes.get(start..start + t.len()) == Some(t.as_bytes()));
            match found {
                Some((t, v)) => {
                    out.push(Redaction {
                        start,
                        end: start + t.len(),
                        with: v.clone(),
                    });
                    i = start + t.len();
                }
                None => i = start + 1,
            }
        }
        out
    }

    /// Where a token may be starting at the end of `text` without being complete yet: the byte
    /// offset of that `[`, so nothing from it on is released.
    #[must_use]
    pub fn pending(&self, text: &str) -> Option<usize> {
        let from = text.len().saturating_sub(self.longest);
        let from = (from..=text.len()).find(|&i| text.is_char_boundary(i))?;
        let tail = text.get(from..)?;
        tail.char_indices()
            .filter(|(_, c)| *c == '[')
            .map(|(i, _)| from + i)
            .find(|&start| {
                let rest = text.get(start..).unwrap_or_default();
                self.pairs
                    .iter()
                    .any(|(t, _)| t.len() > rest.len() && t.starts_with(rest))
            })
    }

    /// `text` with every token replaced by its value.
    #[must_use]
    pub fn restore(&self, text: &str) -> Option<String> {
        let found = self.find(text);
        if found.is_empty() {
            return None;
        }
        Some(crate::window::render(text, &found, 0, text.len()))
    }

    /// Restores every output segment of a client-dialect response body; whether any changed.
    pub fn restore_body(&self, body: &mut Value, d: Dialect) -> bool {
        let mut changed = false;
        for mut seg in output_segments(body, d) {
            if let Some(t) = self.restore(&seg.text) {
                seg.text = t;
                write_back(body, &seg);
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn span(start: usize, end: usize, label: &str) -> Span {
        Span {
            start,
            end,
            label: label.into(),
        }
    }

    #[test]
    fn the_same_value_gets_the_same_token_and_client_text_is_never_reused() {
        let t = Tokenizer::default();
        t.saw(["I wrote [EMAIL_1] myself"].into_iter());
        let text = "a@b.co and c@d.co and a@b.co";
        let out = t.apply(
            text,
            &[span(0, 6, "EMAIL"), span(11, 17, "EMAIL"), span(22, 28, "EMAIL")],
        );
        assert_eq!(out, "[EMAIL_2] and [EMAIL_3] and [EMAIL_2]");
        let tokens = t.finish().unwrap();
        assert_eq!(
            tokens.restore("write to [EMAIL_3] or [EMAIL_2]").as_deref(),
            Some("write to c@d.co or a@b.co")
        );
        assert_eq!(tokens.restore("[EMAIL_1] stays"), None);
    }

    #[test]
    fn a_token_cut_at_the_end_of_the_text_is_pending() {
        let tokens = Tokens::new(vec![("[EMAIL_1]".into(), "a@b.co".into())]);
        assert_eq!(tokens.pending("mail [EMA"), Some(5));
        assert_eq!(tokens.pending("mail ["), Some(5));
        assert_eq!(tokens.pending("mail [x"), None);
        assert_eq!(tokens.pending("mail [EMAIL_1]"), None);
        assert_eq!(tokens.pending("plain"), None);
    }

    #[test]
    fn bodies_are_restored_in_every_output_segment() {
        let tokens = Tokens::new(vec![("[PHONE_1]".into(), "+44 20 7946 0958".into())]);
        let mut body = json!({"choices": [{"index": 0, "message": {"role": "assistant",
            "content": "call [PHONE_1]",
            "tool_calls": [{"id": "c", "type": "function", "function": {"name": "dial", "arguments": "{\"to\":\"[PHONE_1]\"}"}}]}}]});
        assert!(tokens.restore_body(&mut body, Dialect::Chat));
        assert_eq!(body["choices"][0]["message"]["content"], "call +44 20 7946 0958");
        assert_eq!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"to\":\"+44 20 7946 0958\"}"
        );
    }
}

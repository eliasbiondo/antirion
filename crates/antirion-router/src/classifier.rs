//! Keyword and regex classifiers, and the routing text they read: by default the last non-empty
//! user text, skipping tool results and assistant turns.

use regex::{Regex, RegexBuilder};
use serde_json::Value;

/// Where the routing text comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    /// The last `turns` user texts (1 for `last_user_text`).
    pub turns: usize,
    pub include_system: bool,
    pub max_chars: usize,
}

impl Default for Input {
    fn default() -> Input {
        Input {
            turns: 1,
            include_system: false,
            max_chars: 16_384,
        }
    }
}

impl Input {
    /// Reads a node's `input` member.
    #[must_use]
    pub fn from_value(v: Option<&Value>) -> Input {
        let d = Input::default();
        let Some(v) = v else { return d };
        let window = v.get("source").and_then(Value::as_str) == Some("user_window");
        Input {
            turns: if window {
                v.get("turns")
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(1)
                    .max(1)
            } else {
                1
            },
            include_system: v.get("include_system").and_then(Value::as_bool).unwrap_or(false),
            max_chars: v
                .get("max_chars")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(d.max_chars),
        }
    }
}

/// The text parts of a message `content` (a string, or a list of parts); tool results and media
/// are skipped.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| {
                matches!(
                    p.get("type").and_then(Value::as_str),
                    Some("text" | "input_text") | None
                )
            })
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The routing text of a request body in any dialect; `None` when it has no user text.
#[must_use]
pub fn routing_text(body: &Value, input: &Input) -> Option<String> {
    let mut system: Vec<String> = Vec::new();
    let mut users: Vec<String> = Vec::new();
    if let Some(s) = body.get("system") {
        system.push(text_of(s));
    }
    if let Some(s) = body.get("instructions").and_then(Value::as_str) {
        system.push(s.to_owned());
    }
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .or_else(|| body.get("input").and_then(Value::as_array));
    match (messages, body.get("input")) {
        (Some(list), _) => {
            for m in list {
                let role = m.get("role").and_then(Value::as_str).unwrap_or_default();
                let content = m.get("content").unwrap_or(&Value::Null);
                match role {
                    "system" | "developer" => system.push(text_of(content)),
                    "user" => {
                        let t = text_of(content);
                        if !t.trim().is_empty() {
                            users.push(t);
                        }
                    }
                    _ => {}
                }
            }
        }
        (None, Some(Value::String(s))) if !s.trim().is_empty() => users.push(s.clone()),
        _ => {}
    }
    let start = users.len().saturating_sub(input.turns);
    let tail = users.get(start..)?;
    if tail.is_empty() {
        return None;
    }
    let mut text = tail.join("\n");
    if input.include_system && !system.is_empty() {
        text = format!("{}\n{text}", system.join("\n"));
    }
    Some(clip(&text, input.max_chars))
}

fn clip(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let half = max / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text.chars().skip(count - half).collect();
    format!("{head}…{tail}")
}

/// A compiled keyword or regex classifier: classes in priority order.
#[derive(Clone, Debug)]
pub struct Matcher {
    classes: Vec<Vec<Regex>>,
}

/// Why a classifier does not compile: the class index and pattern index of the bad pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadPattern {
    pub class: usize,
    pub pattern: usize,
}

impl Matcher {
    /// Whole-word, case-insensitive keyword matching.
    pub fn keywords(classes: &[Vec<String>]) -> Result<Matcher, BadPattern> {
        Matcher::build(classes, |k| format!(r"(?i)\b{}\b", regex::escape(k)))
    }

    /// Unanchored regular expressions.
    pub fn patterns(classes: &[Vec<String>]) -> Result<Matcher, BadPattern> {
        Matcher::build(classes, str::to_owned)
    }

    fn build(classes: &[Vec<String>], source: impl Fn(&str) -> String) -> Result<Matcher, BadPattern> {
        let mut out = Vec::new();
        for (ci, items) in classes.iter().enumerate() {
            let mut compiled = Vec::new();
            for (pi, item) in items.iter().enumerate() {
                let re = RegexBuilder::new(&source(item))
                    .size_limit(1 << 20)
                    .build()
                    .map_err(|_| BadPattern {
                        class: ci,
                        pattern: pi,
                    })?;
                compiled.push(re);
            }
            out.push(compiled);
        }
        Ok(Matcher { classes: out })
    }

    /// The first class that matches the text.
    #[must_use]
    pub fn classify(&self, text: &str) -> Option<usize> {
        self.classes
            .iter()
            .position(|c| c.iter().any(|re| re.is_match(text)))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn keywords_are_whole_words_and_ordered() {
        let m =
            Matcher::keywords(&[vec!["python".into(), "function".into()], vec!["translate".into()]]).unwrap();
        assert_eq!(m.classify("write a python function"), Some(0));
        assert_eq!(m.classify("Translate to French"), Some(1));
        assert_eq!(m.classify("translate this python snippet"), Some(0));
        assert_eq!(m.classify("pythonic idioms"), None);
    }

    #[test]
    fn routing_text_skips_tool_and_assistant_turns() {
        let body = json!({"messages": [
            {"role": "user", "content": "code please"},
            {"role": "assistant", "content": "ok"},
            {"role": "tool", "tool_call_id": "c", "content": "hello"}]});
        assert_eq!(
            routing_text(&body, &Input::default()).as_deref(),
            Some("code please")
        );
        let blank =
            json!({"messages": [{"role": "user", "content": "code"}, {"role": "user", "content": "   "}]});
        assert_eq!(routing_text(&blank, &Input::default()).as_deref(), Some("code"));
        let image = json!({"messages": [{"role": "user", "content": [{"type": "image_url", "image_url": {"url": "x"}}]}]});
        assert_eq!(routing_text(&image, &Input::default()), None);
        let parts = json!({"messages": [{"role": "user", "content": [{"type": "text", "text": "code"}, {"type": "image_url"}]}]});
        assert_eq!(routing_text(&parts, &Input::default()).as_deref(), Some("code"));
        let anthropic =
            json!({"messages": [{"role": "user", "content": [{"type": "tool_result", "content": "x"}]}]});
        assert_eq!(routing_text(&anthropic, &Input::default()), None);
    }
}

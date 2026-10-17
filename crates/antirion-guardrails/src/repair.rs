//! JSON repair of model output: content that should be JSON and is not is cut out of code fences
//! and surrounding prose, trailing commas are dropped and bare keys quoted. Content that still does
//! not parse is left as it was.

use serde_json::Value;

use crate::segments::Dialect;

/// Whether a request asked for JSON output: Chat `response_format` and Responses `text.format` of
/// type `json_object` or `json_schema`, Anthropic `output_format` of type `json_schema`, Gemini
/// `generationConfig.responseMimeType` `application/json`.
#[must_use]
pub fn wants_json(body: &Value, d: Dialect) -> bool {
    let json_type = |v: Option<&Value>| {
        matches!(
            v.and_then(|f| f.get("type")).and_then(Value::as_str),
            Some("json_object" | "json_schema")
        )
    };
    match d {
        Dialect::Chat | Dialect::Completions => json_type(body.get("response_format")),
        Dialect::Responses => json_type(body.pointer("/text/format")),
        Dialect::Anthropic => json_type(body.get("output_format")),
        Dialect::Gemini => {
            body.pointer("/generationConfig/responseMimeType")
                .and_then(Value::as_str)
                == Some("application/json")
        }
        _ => false,
    }
}

fn parses(text: &str) -> bool {
    serde_json::from_str::<Value>(text).is_ok()
}

/// The inside of the first fenced code block, when the text has one.
fn unfenced(text: &str) -> Option<&str> {
    let open = text.find("```")?;
    let after = text.get(open + 3..)?;
    let body_start = after.find('\n').map_or(0, |n| n + 1);
    let body = after.get(body_start..)?;
    let close = body.find("```").unwrap_or(body.len());
    body.get(..close)
}

/// From the first `{` or `[` to the last matching closer.
fn outer(text: &str) -> Option<&str> {
    let start = text.find(['{', '['])?;
    let closer = if text.get(start..)?.starts_with('{') {
        '}'
    } else {
        ']'
    };
    let end = text.rfind(closer)?;
    (end > start).then(|| text.get(start..=end)).flatten()
}

/// Drops commas before `}` or `]` and quotes bare object keys, outside strings.
fn mend(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
                i += 1;
            }
            ',' => {
                let next = chars
                    .get(i + 1..)
                    .and_then(|rest| rest.iter().find(|c| !c.is_whitespace()));
                if !matches!(next, Some('}' | ']')) {
                    out.push(c);
                }
                i += 1;
            }
            c if (c.is_ascii_alphabetic() || c == '_' || c == '$') && key_position(&out) => {
                let start = i;
                while chars
                    .get(i)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
                {
                    i += 1;
                }
                let word: String = chars.get(start..i).unwrap_or_default().iter().collect();
                let colon = chars
                    .get(i..)
                    .and_then(|rest| rest.iter().find(|c| !c.is_whitespace()))
                    == Some(&':');
                if colon {
                    out.push('"');
                    out.push_str(&word);
                    out.push('"');
                } else {
                    out.push_str(&word);
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Whether what was written so far ends where an object key starts (after `{` or `,`).
fn key_position(written: &str) -> bool {
    matches!(written.trim_end().chars().last(), Some('{' | ','))
}

/// The repaired content, when `text` is not valid JSON and the repair makes it valid.
#[must_use]
pub fn repair(text: &str) -> Option<String> {
    if parses(text) {
        return None;
    }
    let candidate = unfenced(text).unwrap_or(text).trim();
    if parses(candidate) {
        return Some(candidate.to_owned());
    }
    let candidate = outer(candidate).unwrap_or(candidate);
    if parses(candidate) {
        return Some(candidate.to_owned());
    }
    let mended = mend(candidate);
    parses(&mended).then_some(mended)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fixed(text: &str) -> Value {
        serde_json::from_str(&repair(text).unwrap()).unwrap()
    }

    #[test]
    fn fences_prose_trailing_commas_and_bare_keys_are_repaired() {
        assert_eq!(fixed("```json\n{\"a\":1,}\n```"), json!({"a": 1}));
        assert_eq!(
            fixed("Here you go: {\"a\": [1, 2,],} Hope it helps."),
            json!({"a": [1, 2]})
        );
        assert_eq!(
            fixed("{a: 1, b_2: \"x, }\", $c: {d: true,},}"),
            json!({"a": 1, "b_2": "x, }", "$c": {"d": true}})
        );
        assert_eq!(fixed("```\n[1, 2,]\n```"), json!([1, 2]));
    }

    #[test]
    fn valid_json_and_hopeless_text_are_left_alone() {
        assert_eq!(repair("{\"a\": 1}"), None);
        assert_eq!(repair("no json here"), None);
        assert_eq!(repair("{a: undefined}"), None);
    }

    #[test]
    fn json_output_is_read_per_dialect() {
        assert!(wants_json(
            &json!({"response_format": {"type": "json_object"}}),
            Dialect::Chat
        ));
        assert!(!wants_json(
            &json!({"response_format": {"type": "text"}}),
            Dialect::Chat
        ));
        assert!(wants_json(
            &json!({"text": {"format": {"type": "json_schema"}}}),
            Dialect::Responses
        ));
        assert!(wants_json(
            &json!({"generationConfig": {"responseMimeType": "application/json"}}),
            Dialect::Gemini
        ));
    }
}

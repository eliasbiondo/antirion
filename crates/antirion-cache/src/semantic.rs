//! The semantic layer's rules: which requests it serves, the text that is embedded, the body its
//! partition key is computed from, and the polarity tokens that keep a negation from matching its
//! affirmation.

use std::collections::HashSet;

use serde_json::Value;

/// The default polarity lexicon: English negations and antonym pairs. Any word ending in `n't`
/// also counts.
const NEGATIONS: &[&str] = &["not", "no", "never", "none", "nothing", "nobody", "without"];
const ANTONYMS: &[(&str, &str)] = &[
    ("on", "off"),
    ("enable", "disable"),
    ("enabled", "disabled"),
    ("allow", "deny"),
    ("start", "stop"),
    ("open", "close"),
    ("increase", "decrease"),
    ("include", "exclude"),
    ("before", "after"),
    ("true", "false"),
    ("yes", "no"),
    ("add", "remove"),
    ("lock", "unlock"),
    ("buy", "sell"),
    ("accept", "reject"),
];

/// The words whose presence and count must match between a request and a candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lexicon {
    words: HashSet<String>,
    /// Words ending in `n't` count (the default lexicon).
    contractions: bool,
}

impl Lexicon {
    /// The default lexicon.
    #[must_use]
    pub fn standard() -> Lexicon {
        let words = NEGATIONS
            .iter()
            .copied()
            .chain(ANTONYMS.iter().flat_map(|(a, b)| [*a, *b]))
            .map(str::to_owned)
            .collect();
        Lexicon {
            words,
            contractions: true,
        }
    }

    /// A configured lexicon: words, and `a/b` antonym pairs whose both sides count.
    #[must_use]
    pub fn of(entries: &[String]) -> Lexicon {
        let words = entries
            .iter()
            .flat_map(|e| e.split('/'))
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect();
        Lexicon {
            words,
            contractions: false,
        }
    }

    /// The lexicon of `polarity_lexicon`: `"default"`, or a list.
    #[must_use]
    pub fn from_value(v: Option<&Value>) -> Lexicon {
        match v.and_then(Value::as_array) {
            Some(list) => {
                let entries: Vec<String> = list.iter().filter_map(Value::as_str).map(str::to_owned).collect();
                Lexicon::of(&entries)
            }
            None => Lexicon::standard(),
        }
    }

    /// The polarity tokens of `text`: its case-folded whole words in the lexicon, sorted, with
    /// repeats (a multiset).
    #[must_use]
    pub fn tokens(&self, text: &str) -> Vec<String> {
        let mut out: Vec<String> = text
            .split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '\u{2019}'))
            .map(|w| {
                w.trim_matches(['\'', '\u{2019}'])
                    .replace('\u{2019}', "'")
                    .to_lowercase()
            })
            .filter(|w| !w.is_empty())
            .filter(|w| self.words.contains(w) || (self.contractions && w.ends_with("n't")))
            .collect();
        out.sort_unstable();
        out
    }
}

/// What the semantic layer needs from an eligible request.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    /// The embedding input: the last user message's text parts joined with `"\n"`.
    pub text: String,
    /// The body with the last user message's text emptied, which the partition key is built from.
    pub partition_body: Value,
}

/// Whether a content part is text.
fn text_part(p: &Value) -> bool {
    matches!(
        p.get("type").and_then(Value::as_str),
        Some("text" | "input_text" | "output_text")
    )
}

/// The texts of a message's content (a string, or text parts only); `None` when it holds any
/// other part.
fn texts(content: Option<&Value>) -> Option<Vec<&str>> {
    match content {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(Value::String(s)) => Some(vec![s.as_str()]),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|p| {
                text_part(p)
                    .then(|| p.get("text").and_then(Value::as_str))
                    .flatten()
            })
            .collect(),
        Some(_) => None,
    }
}

/// Empties the text of a message's content in place.
fn empty_texts(content: &mut Value) {
    match content {
        Value::String(s) => s.clear(),
        Value::Array(parts) => {
            for p in parts {
                if let Some(Value::String(t)) = p.get_mut("text") {
                    t.clear();
                }
            }
        }
        _ => {}
    }
}

/// The eligible request of a messages list (Chat, Anthropic, Responses items): every message
/// text-only, the last one from the user.
fn conversation(body: &Value, list: &str, max_chars: usize) -> Option<Query> {
    let messages = body.get(list)?.as_array()?;
    for m in messages {
        if list == "input"
            && m.get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| t != "message")
        {
            return None;
        }
        texts(m.get("content"))?;
    }
    let last = messages.last()?;
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let text = texts(last.get("content"))?.join("\n");
    if text.chars().count() > max_chars {
        return None;
    }
    let mut partition_body = body.clone();
    if let Some(content) = partition_body
        .get_mut(list)
        .and_then(Value::as_array_mut)
        .and_then(|a| a.last_mut())
        .and_then(|m| m.get_mut("content"))
    {
        empty_texts(content);
    }
    Some(Query { text, partition_body })
}

/// A request whose only input is one string member (`prompt`, a string `input`).
fn single(body: &Value, member: &str, max_chars: usize) -> Option<Query> {
    let text = body.get(member)?.as_str()?;
    if text.chars().count() > max_chars {
        return None;
    }
    let mut partition_body = body.clone();
    if let Some(o) = partition_body.as_object_mut() {
        o.insert(member.to_owned(), Value::String(String::new()));
    }
    Some(Query {
        text: text.to_owned(),
        partition_body,
    })
}

/// The semantic query of a request to the operation `op`, or `None` when it is not eligible: only
/// text anywhere, the last message from the user, its text at most `max_chars` code points.
#[must_use]
pub fn eligible(op: &str, body: &Value, max_chars: usize) -> Option<Query> {
    match op {
        "openai.chat_completions" | "anthropic.messages" => {
            if op == "anthropic.messages" && texts(body.get("system")).is_none() {
                return None;
            }
            conversation(body, "messages", max_chars)
        }
        "openai.completions" => single(body, "prompt", max_chars),
        "openai.responses" => match body.get("input") {
            Some(Value::String(_)) => single(body, "input", max_chars),
            _ => conversation(body, "input", max_chars),
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn polarity_tokens_are_a_multiset_of_lexicon_words() {
        let l = Lexicon::standard();
        assert_eq!(l.tokens("How do I turn ON the lights?"), ["on"]);
        assert_eq!(l.tokens("turn off the lights"), ["off"]);
        assert_eq!(l.tokens("Don't stop, no, NO"), ["don't", "no", "no", "stop"]);
        assert_eq!(l.tokens("it won\u{2019}t open"), ["open", "won't"]);
        assert!(l.tokens("the capital of france").is_empty());
        let custom = Lexicon::of(&["up/down".into(), "Maybe".into()]);
        assert_eq!(custom.tokens("maybe go down, not up"), ["down", "maybe", "up"]);
        assert_eq!(Lexicon::from_value(Some(&json!("default"))), Lexicon::standard());
    }

    #[test]
    fn eligibility_and_the_partition_body() {
        let chat = json!({"model": "m", "messages": [
            {"role": "system", "content": "be brief"},
            {"role": "user", "content": [{"type": "text", "text": "capital"}, {"type": "text", "text": "of France"}]}]});
        let q = eligible("openai.chat_completions", &chat, 8192).unwrap();
        assert_eq!(q.text, "capital\nof France");
        assert_eq!(q.partition_body["messages"][1]["content"][1]["text"], "");
        assert_eq!(q.partition_body["messages"][0]["content"], "be brief");
        assert!(
            eligible("openai.chat_completions", &chat, 10).is_none(),
            "too long"
        );

        let image = json!({"messages": [{"role": "user", "content": [
            {"type": "text", "text": "what is this"}, {"type": "image_url", "image_url": {"url": "x"}}]}]});
        assert!(eligible("openai.chat_completions", &image, 8192).is_none());
        let assistant_last =
            json!({"messages": [{"role": "user", "content": "a"}, {"role": "assistant", "content": "b"}]});
        assert!(eligible("openai.chat_completions", &assistant_last, 8192).is_none());
        let earlier_image = json!({"messages": [
            {"role": "user", "content": [{"type": "image", "source": {}}]},
            {"role": "assistant", "content": "ok"}, {"role": "user", "content": "and now"}]});
        assert!(eligible("anthropic.messages", &earlier_image, 8192).is_none());

        assert_eq!(
            eligible("openai.responses", &json!({"input": "hi"}), 10)
                .unwrap()
                .text,
            "hi"
        );
        let call = json!({"input": [{"type": "function_call_output", "output": "x"}, {"role": "user", "content": "hi"}]});
        assert!(eligible("openai.responses", &call, 10).is_none());
        assert_eq!(
            eligible("openai.completions", &json!({"prompt": "p"}), 10)
                .unwrap()
                .partition_body["prompt"],
            ""
        );
        assert!(eligible("openai.completions", &json!({"prompt": ["a", "b"]}), 10).is_none());
        assert!(eligible("openai.embeddings", &json!({"input": "x"}), 10).is_none());
    }
}

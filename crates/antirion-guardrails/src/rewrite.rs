//! `llm_rewrite`: the segments in scope go to a Model with the operator's prompt, which answers
//! `{"segments": [{"id", "text"}]}`; each returned text replaces its segment.

use std::collections::BTreeMap;

use antirion_config::{ConfigError, DocPath};
use serde_json::{Value, json};

/// An `llm_rewrite` guardrail's settings.
#[derive(Clone, Debug)]
pub struct Rewrite {
    /// The public Model asked.
    pub model: String,
    /// The operator prompt, sent as the system message.
    pub prompt: String,
    pub max_tokens: u32,
}

impl Rewrite {
    pub(crate) fn compile(
        b: Option<&Value>,
        at: &DocPath,
        doc: &Value,
        errors: &mut Vec<ConfigError>,
    ) -> Option<Rewrite> {
        let b = b?;
        let model = b.get("model")?.as_str()?.to_owned();
        crate::services::model_exists(doc, &model, &at.key("llm_rewrite").key("model"), errors);
        Some(Rewrite {
            model,
            prompt: b.get("prompt")?.as_str()?.to_owned(),
            max_tokens: b
                .get("max_tokens")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(1024),
        })
    }

    /// The user message: the texts as `{"segments": [{"id": "s0", "text": …}, …]}`.
    #[must_use]
    pub fn message(texts: &[&str]) -> String {
        let segments: Vec<Value> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| json!({"id": format!("s{i}"), "text": t}))
            .collect();
        json!({"segments": segments}).to_string()
    }
}

/// Reads a rewrite answer for `n` segments: the new text by segment number. An answer that is not
/// the JSON object (in one code fence at most), an unknown id or an id given twice is an error.
pub fn rewritten(answer: &str, n: usize) -> Result<BTreeMap<usize, String>, String> {
    let trimmed = answer.trim();
    let text = trimmed
        .strip_prefix("```")
        .and_then(|t| t.strip_suffix("```"))
        .map_or(trimmed, |t| t.split_once('\n').map_or(t, |(_, rest)| rest));
    let v: Value =
        serde_json::from_str(text.trim()).map_err(|_| "the rewrite answer is not JSON".to_owned())?;
    let items = v
        .get("segments")
        .and_then(Value::as_array)
        .ok_or_else(|| "the rewrite answer has no segments array".to_owned())?;
    let mut out = BTreeMap::new();
    for item in items {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "a rewritten segment has no id".to_owned())?;
        let text = item
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("rewritten segment {id} has no text"))?;
        let i = id
            .strip_prefix('s')
            .and_then(|d| d.parse::<usize>().ok())
            .filter(|i| *i < n)
            .ok_or_else(|| format!("unknown segment {id}"))?;
        if out.insert(i, text.to_owned()).is_some() {
            return Err(format!("segment {id} rewritten twice"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_read_strictly() {
        assert_eq!(
            Rewrite::message(&["a \"b\"", "c"]),
            r#"{"segments":[{"id":"s0","text":"a \"b\""},{"id":"s1","text":"c"}]}"#
        );
        let got = rewritten(
            "```json\n{\"segments\": [{\"id\": \"s1\", \"text\": \"x\"}]}\n```",
            2,
        )
        .unwrap();
        assert_eq!(got.into_iter().collect::<Vec<_>>(), [(1, "x".to_owned())]);
        assert!(rewritten("sure! here it is", 1).is_err());
        assert!(rewritten(r#"{"segments": [{"id": "s3", "text": "x"}]}"#, 2).is_err());
        assert!(
            rewritten(
                r#"{"segments": [{"id": "s0", "text": "x"}, {"id": "s0", "text": "y"}]}"#,
                1
            )
            .is_err()
        );
        assert!(rewritten(r#"{"rewritten": []}"#, 1).is_err());
    }
}

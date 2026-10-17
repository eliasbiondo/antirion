//! The `json_schema` check: each choice's content must parse as JSON and satisfy a schema, the
//! configured one or the one the request asked the model to follow.

use std::sync::Arc;

use antirion_config::schema::UserSchema;
use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::Value;

use crate::def::Category;
use crate::segments::Dialect;

/// A `json_schema` guardrail's settings.
#[derive(Clone, Debug)]
pub struct JsonSchemaCheck {
    /// The configured schema.
    pub schema: Option<Arc<UserSchema>>,
    /// Validate against the request's own schema when it has one.
    pub use_request_schema: bool,
    /// How many times a response that fails is asked for again.
    pub reask: u32,
}

/// The schema a request asks for, compiled once per request; an error when it cannot be used.
pub type RequestSchema = Result<Arc<UserSchema>, String>;

impl JsonSchemaCheck {
    pub(crate) fn compile(
        block: Option<&Value>,
        at: &DocPath,
        errors: &mut Vec<ConfigError>,
    ) -> JsonSchemaCheck {
        let use_request_schema = block
            .and_then(|b| b.get("use_request_schema"))
            .and_then(Value::as_bool)
            == Some(true);
        let schema = match block.and_then(|b| b.get("schema")) {
            Some(s) => match UserSchema::compile(s) {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    errors.push(ConfigError::new(
                        &at.key("json_schema").key("schema"),
                        DetailCode::Conflict,
                        format!("the schema cannot be used: {e}"),
                    ));
                    None
                }
            },
            None => {
                if !use_request_schema {
                    errors.push(ConfigError::new(
                        &at.key("json_schema").key("schema"),
                        DetailCode::Required,
                        "a json_schema guardrail needs a schema unless it uses the request's",
                    ));
                }
                None
            }
        };
        JsonSchemaCheck {
            schema,
            use_request_schema,
            reask: block
                .and_then(|b| b.get("reask"))
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0),
        }
    }

    /// The finding for a response: its category and detail, or `None` when every choice's content
    /// is valid. `Err` when the request's schema was to be used and cannot be.
    pub fn verdict(
        &self,
        body: &Value,
        d: Dialect,
        request: Option<&RequestSchema>,
    ) -> Result<Option<(Category, String)>, String> {
        let schema = match (self.use_request_schema, request) {
            (true, Some(Ok(s))) => Some(Arc::clone(s)),
            (true, Some(Err(e))) => return Err(e.clone()),
            _ => self.schema.clone(),
        };
        let Some(schema) = schema else {
            return Ok(None);
        };
        for (i, text) in choice_contents(body, d).iter().enumerate() {
            match serde_json::from_str::<Value>(text) {
                Err(_) => return Ok(Some((Category::InvalidJson, format!("choice {i}")))),
                Ok(v) if !schema.accepts(&v) => {
                    return Ok(Some((Category::SchemaViolation, format!("choice {i}"))));
                }
                Ok(_) => {}
            }
        }
        Ok(None)
    }
}

/// The schema a request body asks the model to follow: Chat `response_format.json_schema.schema`,
/// Responses `text.format.schema` with `type: json_schema`.
#[must_use]
pub fn requested(body: &Value, d: Dialect) -> Option<RequestSchema> {
    let schema = match d {
        Dialect::Chat => body.pointer("/response_format/json_schema/schema"),
        Dialect::Responses => body
            .pointer("/text/format")
            .filter(|f| f.get("type").and_then(Value::as_str) == Some("json_schema"))
            .and_then(|f| f.get("schema")),
        _ => None,
    }?;
    Some(UserSchema::compile(schema).map(Arc::new))
}

fn text_of(content: Option<&Value>, part_type: &str) -> Option<String> {
    match content? {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some(part_type))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect(),
        ),
        _ => None,
    }
}

/// Each choice's text content, in order (one entry for dialects with a single choice).
#[must_use]
pub fn choice_contents(body: &Value, d: Dialect) -> Vec<String> {
    let arr = |v: Option<&Value>| v.and_then(Value::as_array).cloned().unwrap_or_default();
    match d {
        Dialect::Chat | Dialect::Completions => arr(body.get("choices"))
            .iter()
            .filter_map(|c| {
                c.get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| text_of(c.pointer("/message/content"), "text"))
            })
            .collect(),
        Dialect::Anthropic => text_of(body.get("content"), "text").into_iter().collect(),
        Dialect::Responses => {
            let text: String = arr(body.get("output"))
                .iter()
                .filter(|i| i.get("type").and_then(Value::as_str) == Some("message"))
                .filter_map(|i| text_of(i.get("content"), "output_text"))
                .collect();
            vec![text]
        }
        Dialect::Gemini => arr(body.get("candidates"))
            .iter()
            .map(|c| {
                arr(c.pointer("/content/parts"))
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect()
            })
            .collect(),
        Dialect::Embeddings | Dialect::Images | Dialect::Rerank | Dialect::Transcription => {
            crate::endpoints::output(body, d)
                .into_iter()
                .map(|s| s.text)
                .collect()
        }
        Dialect::Converse => vec![
            arr(body.pointer("/output/message/content"))
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn check(block: Value) -> JsonSchemaCheck {
        let mut errors = Vec::new();
        let c = JsonSchemaCheck::compile(Some(&block), &DocPath::root(), &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        c
    }

    fn chat(content: &str) -> Value {
        json!({"choices": [{"message": {"role": "assistant", "content": content}}]})
    }

    #[test]
    fn content_must_parse_and_satisfy_the_schema() {
        let c = check(json!({"schema": {"type": "object", "required": ["a"]}}));
        assert_eq!(c.verdict(&chat(r#"{"a":1}"#), Dialect::Chat, None), Ok(None));
        assert_eq!(
            c.verdict(&chat(r#"{"b":1}"#), Dialect::Chat, None)
                .unwrap()
                .map(|f| f.0),
            Some(Category::SchemaViolation)
        );
        assert_eq!(
            c.verdict(&chat("not json"), Dialect::Chat, None)
                .unwrap()
                .map(|f| f.0),
            Some(Category::InvalidJson)
        );
    }

    #[test]
    fn the_request_schema_wins_when_asked_for() {
        let c = check(json!({"use_request_schema": true}));
        let body = json!({"response_format": {"type": "json_schema", "json_schema": {"name": "x",
            "schema": {"type": "object", "required": ["b"]}}}});
        let req = requested(&body, Dialect::Chat);
        assert_eq!(
            c.verdict(&chat(r#"{"b":1}"#), Dialect::Chat, req.as_ref()),
            Ok(None)
        );
        assert!(
            c.verdict(&chat(r#"{"a":1}"#), Dialect::Chat, req.as_ref())
                .unwrap()
                .is_some()
        );
        assert_eq!(
            c.verdict(&chat("anything"), Dialect::Chat, None),
            Ok(None),
            "nothing to check against"
        );
        let bad = Some(Err("unsupported".to_owned()));
        assert!(c.verdict(&chat("{}"), Dialect::Chat, bad.as_ref()).is_err());
        let mut errors = Vec::new();
        JsonSchemaCheck::compile(Some(&json!({})), &DocPath::root(), &mut errors);
        assert_eq!(errors.len(), 1);
    }
}

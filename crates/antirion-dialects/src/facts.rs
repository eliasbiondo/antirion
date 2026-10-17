//! Facts read from a request body once at `parse`: the requested model and whether to stream.

use antirion_json::Skim;
use serde_json::Value;

/// The requested model as the body states it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelField {
    /// Absent, `null` or the empty string.
    Missing,
    Name(String),
    /// Present with a type other than string.
    WrongType,
}

/// Facts of an OpenAI-family or Anthropic body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyFacts {
    pub model: ModelField,
    pub stream: bool,
}

fn member_value(raw: &[u8], skim: &Skim, name: &str) -> Option<Value> {
    let m = skim.member(name)?;
    serde_json::from_slice(raw.get(m.value.clone())?).ok()
}

/// Reads `model` and `stream` from a skimmed JSON object body.
#[must_use]
pub fn body_facts(raw: &[u8], skim: &Skim) -> BodyFacts {
    let model = match member_value(raw, skim, "model") {
        None | Some(Value::Null) => ModelField::Missing,
        Some(Value::String(s)) if s.is_empty() => ModelField::Missing,
        Some(Value::String(s)) => ModelField::Name(s),
        Some(_) => ModelField::WrongType,
    };
    let stream = matches!(member_value(raw, skim, "stream"), Some(Value::Bool(true)));
    BodyFacts { model, stream }
}

#[cfg(test)]
mod tests {
    use antirion_json::{JsonLimits, skim};

    use super::*;

    fn facts(body: &[u8]) -> BodyFacts {
        body_facts(body, &skim(body, &JsonLimits::default()).unwrap())
    }

    #[test]
    fn model_and_stream() {
        assert_eq!(
            facts(br#"{"model":"m","stream":true}"#),
            BodyFacts {
                model: ModelField::Name("m".into()),
                stream: true
            }
        );
        assert_eq!(facts(br#"{"model":""}"#).model, ModelField::Missing);
        assert_eq!(facts(br#"{"model":null}"#).model, ModelField::Missing);
        assert_eq!(facts(br#"{"model":5}"#).model, ModelField::WrongType);
        assert_eq!(facts(br#"{"model":"a/b"}"#).model, ModelField::Name("a/b".into()));
        assert!(!facts(br#"{"model":"m"}"#).stream);
    }
}

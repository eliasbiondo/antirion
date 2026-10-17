//! The checks that ask a service: `llm_judge` (a Model asked for a verdict), `classifier` (a
//! text-classification endpoint) and the partner services. This module holds their settings, how
//! they are compiled, and the judge's and classifier's request and answer.

use std::collections::BTreeMap;

use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::{Value, json};

use crate::def::Category;
use crate::partners::{Partner, PartnerKind};

/// The placeholder a judge prompt puts the inspected text in.
pub const CONTENT_PLACEHOLDER: &str = "{{content}}";

/// An `llm_judge` guardrail.
#[derive(Clone, Debug)]
pub struct Judge {
    /// The public Model asked.
    pub model: String,
    /// The operator prompt, with [`CONTENT_PLACEHOLDER`] where the text goes.
    pub prompt: String,
    pub max_tokens: u32,
}

/// A `classifier` guardrail.
#[derive(Clone, Debug)]
pub struct Classifier {
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// Label → the score at or above which it is a finding.
    pub thresholds: BTreeMap<String, f64>,
    pub max_inputs_per_call: usize,
    pub max_chars: usize,
    pub overlap_chars: usize,
}

/// How a service call is authorised beyond its configured headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceAuth {
    None,
    /// The credentials of a configured provider: AWS SigV4 (`bedrock`, in `region`) or a GCP
    /// access token.
    Provider {
        name: String,
        region: Option<String>,
    },
}

/// One call to a service.
#[derive(Clone, Debug)]
pub struct ServiceCall {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub auth: ServiceAuth,
}

fn s<'a>(v: Option<&'a Value>, key: &str) -> Option<&'a str> {
    v.and_then(|b| b.get(key)).and_then(Value::as_str)
}

fn uint(v: Option<&Value>, key: &str, default: usize) -> usize {
    v.and_then(|b| b.get(key))
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(default)
}

pub(crate) fn headers(v: Option<&Value>) -> Vec<(String, String)> {
    v.and_then(|b| b.get("headers"))
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
        .collect()
}

pub(crate) fn model_exists(doc: &Value, model: &str, at: &DocPath, errors: &mut Vec<ConfigError>) {
    if doc.get("models").and_then(|m| m.get(model)).is_none() {
        errors.push(ConfigError::new(
            at,
            DetailCode::UnresolvedReference,
            format!("model {model:?} does not exist"),
        ));
    }
}

/// Compiles an `llm_judge` block.
pub(crate) fn judge(
    b: Option<&Value>,
    at: &DocPath,
    doc: &Value,
    errors: &mut Vec<ConfigError>,
) -> Option<Judge> {
    let at = at.key("llm_judge");
    let model = s(b, "model")?.to_owned();
    model_exists(doc, &model, &at.key("model"), errors);
    let prompt = s(b, "prompt")?.to_owned();
    if !prompt.contains(CONTENT_PLACEHOLDER) {
        errors.push(ConfigError::new(
            &at.key("prompt"),
            DetailCode::InvalidFormat,
            "the prompt must contain {{content}}",
        ));
    }
    Some(Judge {
        model,
        prompt,
        max_tokens: u32::try_from(uint(b, "max_tokens", 256)).unwrap_or(256),
    })
}

/// Compiles a `classifier` block.
pub(crate) fn classifier(
    b: Option<&Value>,
    at: &DocPath,
    errors: &mut Vec<ConfigError>,
) -> Option<Classifier> {
    let max_chars = uint(b, "max_chars", 8000);
    let overlap = uint(b, "overlap_chars", 200);
    if overlap >= max_chars {
        errors.push(ConfigError::new(
            &at.key("classifier").key("overlap_chars"),
            DetailCode::OutOfRange,
            "overlap_chars must be smaller than max_chars",
        ));
    }
    let thresholds = b
        .and_then(|b| b.get("thresholds"))
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| v.as_f64().map(|v| (k.clone(), v)))
        .collect();
    Some(Classifier {
        url: s(b, "url")?.to_owned(),
        headers: headers(b),
        thresholds,
        max_inputs_per_call: uint(b, "max_inputs_per_call", 32).max(1),
        max_chars: max_chars.max(1),
        overlap_chars: overlap.min(max_chars.saturating_sub(1)),
    })
}

/// Compiles a partner block; a referenced provider must exist and be of the type the service
/// signs with.
pub(crate) fn partner(
    kind: PartnerKind,
    b: Option<&Value>,
    at: &DocPath,
    doc: &Value,
    errors: &mut Vec<ConfigError>,
) -> Option<Partner> {
    let at = at.key(kind.type_name());
    let mut block = b?.clone();
    let provider = s(Some(&block), "provider")
        .and_then(|name| doc.get("providers").and_then(|ps| ps.get(name)))
        .cloned();
    let defaults: &[(&str, &str)] = match kind {
        PartnerKind::Bedrock => &[("region", "/aws/region")],
        PartnerKind::ModelArmor => &[("project", "/gcp/project"), ("location", "/gcp/location")],
        _ => &[],
    };
    if let (Some(o), Some(pv)) = (block.as_object_mut(), &provider) {
        for (field, pointer) in defaults {
            if !o.contains_key(*field)
                && let Some(v) = pv.pointer(pointer).filter(|v| v.is_string())
            {
                o.insert((*field).to_owned(), v.clone());
            }
        }
    }
    let Some(p) = Partner::from_block(kind, &block) else {
        errors.push(ConfigError::new(
            &at,
            DetailCode::Required,
            "a required field is missing and the provider does not set it",
        ));
        return None;
    };
    if let Some(name) = &p.provider {
        let ty = provider
            .as_ref()
            .and_then(|pv| pv.get("type"))
            .and_then(Value::as_str);
        match (ty, kind.provider_type()) {
            (None, _) => errors.push(ConfigError::new(
                &at.key("provider"),
                DetailCode::UnresolvedReference,
                format!("provider {name:?} does not exist"),
            )),
            (Some(t), Some(want)) if t != want => errors.push(ConfigError::new(
                &at.key("provider"),
                DetailCode::Conflict,
                format!("the provider must be of type {want}"),
            )),
            _ => {}
        }
    }
    Some(p)
}

/// The response format a judge asks for: `{"passed": boolean, "reason": string}`.
fn verdict_format() -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "guardrail_verdict",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {"passed": {"type": "boolean"}, "reason": {"type": "string"}},
                "required": ["passed", "reason"],
                "additionalProperties": false
            }
        }
    })
}

impl Judge {
    /// The user message: the prompt with the inspected text in place of the placeholder.
    #[must_use]
    pub fn message(&self, texts: &[&str]) -> String {
        self.prompt.replace(CONTENT_PLACEHOLDER, &texts.join("\n\n"))
    }

    /// The OpenAI Chat body of a judge call to upstream model `model`.
    #[must_use]
    pub fn chat_body(&self, model: &str, message: &str) -> Value {
        json!({
            "model": model,
            "temperature": 0,
            "max_tokens": self.max_tokens,
            "messages": [{"role": "user", "content": message}],
            "response_format": verdict_format(),
        })
    }

    /// The Anthropic Messages body of a judge call (no response format: the prompt asks for it).
    #[must_use]
    pub fn messages_body(&self, model: &str, message: &str) -> Value {
        json!({
            "model": model,
            "temperature": 0,
            "max_tokens": self.max_tokens,
            "messages": [{"role": "user", "content": message}],
        })
    }
}

/// A judge's answer: whether the text passed. An answer that is not `{"passed": boolean,
/// "reason"?: string}` is a failure.
pub fn judged(answer: &str) -> Result<bool, String> {
    let v: Value =
        serde_json::from_str(answer.trim()).map_err(|_| "the judge's answer is not JSON".to_owned())?;
    let passed = v
        .get("passed")
        .and_then(Value::as_bool)
        .ok_or_else(|| "the judge's answer has no boolean passed".to_owned())?;
    if v.get("reason").is_some_and(|r| !r.is_string()) {
        return Err("the judge's reason is not a string".to_owned());
    }
    Ok(passed)
}

/// What a classifier found: the label, its score and the category it is reported with.
#[derive(Clone, Debug, PartialEq)]
pub struct Classified {
    pub label: String,
    pub score: f64,
    pub category: Category,
}

impl Classifier {
    /// The request body of one call.
    #[must_use]
    pub fn body(inputs: &[String]) -> Vec<u8> {
        serde_json::to_vec(&json!({ "inputs": inputs })).unwrap_or_default()
    }

    /// Reads one answer for `inputs` texts: per input a list of `{label, score}`. Returns the
    /// highest-scoring label at or above its threshold.
    pub fn read(
        &self,
        status: u16,
        body: &[u8],
        inputs: usize,
        categories: &BTreeMap<String, Category>,
    ) -> Result<Option<Classified>, String> {
        if !(200..300).contains(&status) {
            return Err(format!("the classifier answered with status {status}"));
        }
        let v: Value =
            serde_json::from_slice(body).map_err(|_| "the classifier's answer is not JSON".to_owned())?;
        let rows = v
            .as_array()
            .ok_or_else(|| "the classifier's answer is not a list".to_owned())?;
        if rows.len() != inputs {
            return Err(format!("{} results for {inputs} inputs", rows.len()));
        }
        let mut best: Option<Classified> = None;
        for row in rows {
            let labels = row
                .as_array()
                .ok_or_else(|| "a classifier result is not a list".to_owned())?;
            for l in labels {
                let (Some(label), Some(score)) = (
                    l.get("label").and_then(Value::as_str),
                    l.get("score").and_then(Value::as_f64),
                ) else {
                    return Err("a classifier label has no label or score".to_owned());
                };
                let Some(threshold) = self.thresholds.get(label) else {
                    continue;
                };
                if score >= *threshold && best.as_ref().is_none_or(|b| score > b.score) {
                    best = Some(Classified {
                        label: label.to_owned(),
                        score,
                        category: categories.get(label).copied().unwrap_or(Category::Custom),
                    });
                }
            }
        }
        Ok(best)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_judge_answer_must_hold_a_boolean_passed() {
        assert_eq!(judged(r#"{"passed": false, "reason": "x"}"#), Ok(false));
        assert_eq!(judged(r#"{"passed":true}"#), Ok(true));
        assert!(judged("maybe").is_err());
        assert!(judged(r#"{"passed": "no"}"#).is_err());
        assert!(judged(r#"{"passed": true, "reason": 3}"#).is_err());
    }

    #[test]
    fn the_judge_prompt_holds_the_text() {
        let j = Judge {
            model: "m".into(),
            prompt: "Is this safe? {{content}}".into(),
            max_tokens: 10,
        };
        assert_eq!(j.message(&["a", "b"]), "Is this safe? a\n\nb");
        let body = j.chat_body("up", "q");
        assert_eq!(body["temperature"], 0);
        assert_eq!(
            body["response_format"]["json_schema"]["schema"]["required"][0],
            "passed"
        );
    }

    #[test]
    fn a_classifier_reports_the_highest_label_over_its_threshold() {
        let c = Classifier {
            url: "https://c".into(),
            headers: Vec::new(),
            thresholds: [("jailbreak".to_owned(), 0.9), ("toxic".to_owned(), 0.5)].into(),
            max_inputs_per_call: 8,
            max_chars: 100,
            overlap_chars: 10,
        };
        let cats = [("jailbreak".to_owned(), Category::PromptInjection)].into();
        let body = br#"[[{"label":"jailbreak","score":0.97},{"label":"benign","score":0.03}],[{"label":"toxic","score":0.6}]]"#;
        let found = c.read(200, body, 2, &cats).unwrap().unwrap();
        assert_eq!(found.label, "jailbreak");
        assert_eq!(found.category, Category::PromptInjection);
        let low = br#"[[{"label":"jailbreak","score":0.2}]]"#;
        assert_eq!(c.read(200, low, 1, &cats).unwrap(), None);
        assert!(c.read(200, low, 2, &cats).is_err());
        assert!(c.read(500, low, 1, &cats).is_err());
        let toxic = br#"[[{"label":"toxic","score":0.6}]]"#;
        assert_eq!(
            c.read(200, toxic, 1, &cats).unwrap().unwrap().category,
            Category::Custom
        );
    }
}

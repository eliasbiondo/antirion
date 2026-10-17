//! Partner guardrail services: the request each one takes for a stage's text, and how its answer
//! maps to a verdict. An answer outside the documented shapes, an intervention type not listed
//! here, or a count of masked outputs that differs from the texts sent is a failure.

use antirion_core::Secret;
use serde_json::{Value, json};

use crate::def::{Category, GuardStage};
use crate::moderation::windows;
use crate::services::{ServiceAuth, ServiceCall};

/// The longest text one content-safety call takes, in code points.
const AZURE_MAX_CHARS: usize = 10_000;
const AZURE_OVERLAP: usize = 200;

/// Which partner service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartnerKind {
    Bedrock,
    AzureContentSafety,
    ModelArmor,
    Lakera,
}

impl PartnerKind {
    #[must_use]
    pub fn parse(ty: &str) -> Option<PartnerKind> {
        Some(match ty {
            "bedrock_guardrails" => PartnerKind::Bedrock,
            "azure_content_safety" => PartnerKind::AzureContentSafety,
            "model_armor" => PartnerKind::ModelArmor,
            "lakera" => PartnerKind::Lakera,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn type_name(self) -> &'static str {
        match self {
            PartnerKind::Bedrock => "bedrock_guardrails",
            PartnerKind::AzureContentSafety => "azure_content_safety",
            PartnerKind::ModelArmor => "model_armor",
            PartnerKind::Lakera => "lakera",
        }
    }

    /// The provider type whose credentials authorise the calls, when the service takes one.
    #[must_use]
    pub const fn provider_type(self) -> Option<&'static str> {
        match self {
            PartnerKind::Bedrock => Some("bedrock"),
            PartnerKind::ModelArmor => Some("vertex"),
            PartnerKind::AzureContentSafety | PartnerKind::Lakera => None,
        }
    }
}

/// A partner guardrail's settings (secret references resolved, provider defaults applied).
#[derive(Clone, Debug)]
pub struct Partner {
    pub kind: PartnerKind,
    /// The provider lending its credentials.
    pub provider: Option<String>,
    /// The service's base URL.
    pub base: String,
    pub api_key: Option<Secret>,
    /// Bedrock: guardrail id and version. Model Armor: project, location and template.
    pub path: String,
    /// Bedrock: the signing region.
    pub region: Option<String>,
    pub api_version: String,
    /// Content Safety: the severity at or above which a category is a finding.
    pub threshold: u64,
    pub project_id: Option<String>,
}

/// What a partner said.
#[derive(Clone, Debug, PartialEq)]
pub enum Said {
    Pass,
    Found {
        category: Category,
        detail: String,
        score: Option<f64>,
        confidence: f64,
    },
    /// Bedrock's anonymised texts, one per text sent.
    Anonymized(Vec<String>),
}

fn text<'a>(b: &'a Value, key: &str) -> Option<&'a str> {
    b.get(key).and_then(Value::as_str)
}

fn trim(url: &str) -> String {
    url.trim_end_matches('/').to_owned()
}

fn finding(category: Category, detail: impl Into<String>) -> Said {
    Said::Found {
        category,
        detail: detail.into(),
        score: None,
        confidence: 1.0,
    }
}

impl Partner {
    /// Reads a block whose provider defaults were filled in.
    pub(crate) fn from_block(kind: PartnerKind, b: &Value) -> Option<Partner> {
        let provider = text(b, "provider").map(str::to_owned);
        let mut p = Partner {
            kind,
            provider,
            base: String::new(),
            api_key: text(b, "api_key").map(Secret::new),
            path: String::new(),
            region: text(b, "region").map(str::to_owned),
            api_version: text(b, "api_version").unwrap_or("2024-09-01").to_owned(),
            threshold: b.get("severity_threshold").and_then(Value::as_u64).unwrap_or(4),
            project_id: text(b, "project_id").map(str::to_owned),
        };
        match kind {
            PartnerKind::Bedrock => {
                let region = p.region.clone().unwrap_or_else(|| "us-east-1".to_owned());
                p.base = text(b, "base_url")
                    .map_or_else(|| format!("https://bedrock-runtime.{region}.amazonaws.com"), trim);
                p.path = format!(
                    "/guardrail/{}/version/{}/apply",
                    text(b, "guardrail_id")?,
                    text(b, "guardrail_version").unwrap_or("DRAFT")
                );
                p.region = Some(region);
            }
            PartnerKind::AzureContentSafety => p.base = trim(text(b, "endpoint")?),
            PartnerKind::ModelArmor => {
                let location = text(b, "location")?;
                p.base = text(b, "base_url").map_or_else(
                    || format!("https://modelarmor.{location}.rep.googleapis.com"),
                    trim,
                );
                p.path = format!(
                    "/v1/projects/{}/locations/{location}/templates/{}",
                    text(b, "project")?,
                    text(b, "template")?
                );
            }
            PartnerKind::Lakera => {
                p.base = text(b, "base_url").map_or_else(|| "https://api.lakera.ai".to_owned(), trim);
            }
        }
        Some(p)
    }

    fn call(&self, url: String, body: &Value, headers: Vec<(String, String)>) -> ServiceCall {
        ServiceCall {
            url,
            headers,
            body: serde_json::to_vec(body).unwrap_or_default(),
            auth: match &self.provider {
                Some(name) => ServiceAuth::Provider {
                    name: name.clone(),
                    region: self.region.clone(),
                },
                None => ServiceAuth::None,
            },
        }
    }

    /// The calls that check `texts` at `stage`.
    #[must_use]
    pub fn calls(&self, stage: GuardStage, texts: &[&str]) -> Vec<ServiceCall> {
        let key = || {
            self.api_key
                .as_ref()
                .map(Secret::expose)
                .unwrap_or_default()
                .to_owned()
        };
        match self.kind {
            PartnerKind::Bedrock => {
                let content: Vec<Value> = texts.iter().map(|t| json!({"text": {"text": t}})).collect();
                let source = if stage == GuardStage::Input {
                    "INPUT"
                } else {
                    "OUTPUT"
                };
                let body = json!({"source": source, "content": content});
                vec![self.call(format!("{}{}", self.base, self.path), &body, Vec::new())]
            }
            PartnerKind::AzureContentSafety => texts
                .iter()
                .flat_map(|t| windows(t, AZURE_MAX_CHARS, AZURE_OVERLAP))
                .map(|t| {
                    let url = format!(
                        "{}/contentsafety/text:analyze?api-version={}",
                        self.base, self.api_version
                    );
                    let headers = vec![("ocp-apim-subscription-key".to_owned(), key())];
                    self.call(url, &json!({"text": t}), headers)
                })
                .collect(),
            PartnerKind::ModelArmor => {
                let joined = texts.join("\n\n");
                let (action, body) = if stage == GuardStage::Input {
                    ("sanitizeUserPrompt", json!({"userPromptData": {"text": joined}}))
                } else {
                    (
                        "sanitizeModelResponse",
                        json!({"modelResponseData": {"text": joined}}),
                    )
                };
                vec![self.call(format!("{}{}:{action}", self.base, self.path), &body, Vec::new())]
            }
            PartnerKind::Lakera => {
                let role = if stage == GuardStage::Input {
                    "user"
                } else {
                    "assistant"
                };
                let messages: Vec<Value> = texts
                    .iter()
                    .map(|t| json!({"role": role, "content": t}))
                    .collect();
                let mut body = json!({"messages": messages, "breakdown": true});
                if let (Some(o), Some(p)) = (body.as_object_mut(), &self.project_id) {
                    o.insert("project_id".into(), json!(p));
                }
                let headers = vec![("authorization".to_owned(), format!("Bearer {}", key()))];
                vec![self.call(format!("{}/v2/guard", self.base), &body, headers)]
            }
        }
    }

    /// The verdict of the answers to [`Partner::calls`], in order; `texts` is how many texts
    /// were sent.
    pub fn read(&self, answers: &[(u16, Vec<u8>)], texts: usize) -> Result<Said, String> {
        let mut found: Option<Said> = None;
        for (status, body) in answers {
            if !(200..300).contains(status) {
                return Err(format!("the service answered with status {status}"));
            }
            let v: Value =
                serde_json::from_slice(body).map_err(|_| "the service's answer is not JSON".to_owned())?;
            let said = match self.kind {
                PartnerKind::Bedrock => bedrock(&v, texts)?,
                PartnerKind::AzureContentSafety => azure(&v, self.threshold)?,
                PartnerKind::ModelArmor => model_armor(&v)?,
                PartnerKind::Lakera => lakera(&v)?,
            };
            let stronger = match (&found, &said) {
                (_, Said::Pass) => false,
                (None | Some(Said::Pass), _) => true,
                (Some(Said::Found { score: a, .. }), Said::Found { score: b, .. }) => b > a,
                _ => false,
            };
            if stronger {
                found = Some(said);
            }
        }
        Ok(found.unwrap_or(Said::Pass))
    }
}

fn bedrock_filter(ty: &str) -> Option<Category> {
    Some(match ty {
        "HATE" => Category::Hate,
        "INSULTS" => Category::Harassment,
        "SEXUAL" => Category::Sexual,
        "VIOLENCE" => Category::Violence,
        "MISCONDUCT" => Category::Illicit,
        "PROMPT_ATTACK" => Category::PromptInjection,
        _ => return None,
    })
}

/// Bedrock `ApplyGuardrail`: `NONE`, or `GUARDRAIL_INTERVENED` with assessments whose items are
/// `BLOCKED` (a finding) or `ANONYMIZED` (a redaction when nothing is blocked).
fn bedrock(v: &Value, texts: usize) -> Result<Said, String> {
    match text(v, "action") {
        Some("NONE") => return Ok(Said::Pass),
        Some("GUARDRAIL_INTERVENED") => {}
        _ => return Err("unrecognised guardrail action".to_owned()),
    }
    let mut blocked: Option<(Category, String)> = None;
    let mut anonymized = false;
    let groups: [(&str, &[&str], Option<Category>); 5] = [
        ("topicPolicy", &["topics"], Some(Category::OffTopic)),
        ("contentPolicy", &["filters"], None),
        (
            "wordPolicy",
            &["customWords", "managedWordLists"],
            Some(Category::Keyword),
        ),
        (
            "sensitiveInformationPolicy",
            &["piiEntities", "regexes"],
            Some(Category::Pii),
        ),
        ("contextualGroundingPolicy", &["filters"], Some(Category::Custom)),
    ];
    for a in v
        .get("assessments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for (policy, lists, fixed) in groups {
            for list in lists {
                for item in a
                    .pointer(&format!("/{policy}/{list}"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match text(item, "action") {
                        Some("BLOCKED") => {
                            let ty = text(item, "type").unwrap_or(policy);
                            let category = match fixed {
                                Some(c) => c,
                                None => bedrock_filter(ty)
                                    .ok_or_else(|| format!("unrecognised filter type {ty}"))?,
                            };
                            blocked.get_or_insert((category, ty.to_ascii_lowercase()));
                        }
                        Some("ANONYMIZED") => anonymized = true,
                        Some("NONE") | None => {}
                        Some(other) => return Err(format!("unrecognised intervention {other}")),
                    }
                }
            }
        }
    }
    if let Some((category, detail)) = blocked {
        return Ok(finding(category, detail));
    }
    if anonymized {
        let outputs: Vec<String> = v
            .get("outputs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|o| text(o, "text").unwrap_or_default().to_owned())
            .collect();
        if outputs.len() != texts {
            return Err(format!("{} masked outputs for {texts} texts", outputs.len()));
        }
        return Ok(Said::Anonymized(outputs));
    }
    Ok(finding(Category::Custom, "intervened"))
}

/// Content Safety `text:analyze`: category severities (0–7) and blocklist matches.
fn azure(v: &Value, threshold: u64) -> Result<Said, String> {
    let rows = v
        .get("categoriesAnalysis")
        .and_then(Value::as_array)
        .ok_or_else(|| "the answer has no categoriesAnalysis".to_owned())?;
    let mut best: Option<(u64, Category, &str)> = None;
    for r in rows {
        let name = text(r, "category").unwrap_or_default();
        let category = match name {
            "Hate" => Category::Hate,
            "SelfHarm" => Category::SelfHarm,
            "Sexual" => Category::Sexual,
            "Violence" => Category::Violence,
            other => return Err(format!("unrecognised category {other:?}")),
        };
        let severity = r.get("severity").and_then(Value::as_u64).unwrap_or(0);
        if severity >= threshold && best.is_none_or(|(s, _, _)| severity > s) {
            best = Some((severity, category, name));
        }
    }
    if let Some((severity, category, name)) = best {
        #[allow(clippy::cast_precision_loss)]
        let score = severity as f64;
        return Ok(Said::Found {
            category,
            detail: name.to_ascii_lowercase(),
            score: Some(score),
            confidence: (score / 7.0).min(1.0),
        });
    }
    if v.get("blocklistsMatch")
        .and_then(Value::as_array)
        .is_some_and(|m| !m.is_empty())
    {
        return Ok(finding(Category::Keyword, "blocklist"));
    }
    Ok(Said::Pass)
}

fn matched(v: &Value) -> bool {
    match v {
        Value::Object(o) => o
            .iter()
            .any(|(k, x)| (k == "matchState" && x.as_str() == Some("MATCH_FOUND")) || matched(x)),
        Value::Array(a) => a.iter().any(matched),
        _ => false,
    }
}

/// Model Armor `sanitize…`: the overall match state, then the filter that matched.
fn model_armor(v: &Value) -> Result<Said, String> {
    let r = v
        .get("sanitizationResult")
        .ok_or_else(|| "the answer has no sanitizationResult".to_owned())?;
    match text(r, "filterMatchState") {
        Some("NO_MATCH_FOUND") => return Ok(Said::Pass),
        Some("MATCH_FOUND") => {}
        _ => return Err("unrecognised filter match state".to_owned()),
    }
    for (name, result) in r
        .get("filterResults")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if !matched(result) {
            continue;
        }
        let category = match name.as_str() {
            "pi_and_jailbreak" => Category::PromptInjection,
            "sdp" => Category::Pii,
            "malicious_uris" => Category::Custom,
            "csam" => Category::Sexual,
            "rai" => {
                let types = result
                    .pointer("/raiFilterResult/raiFilterTypeResults")
                    .and_then(Value::as_object);
                let hit = types
                    .into_iter()
                    .flatten()
                    .find(|(_, t)| matched(t))
                    .map(|(k, _)| k.as_str());
                match hit {
                    Some("hate_speech") => Category::Hate,
                    Some("harassment") => Category::Harassment,
                    Some("sexually_explicit") => Category::Sexual,
                    Some("dangerous") => Category::Illicit,
                    Some(other) => return Err(format!("unrecognised filter {other}")),
                    None => Category::Custom,
                }
            }
            other => return Err(format!("unrecognised filter {other}")),
        };
        return Ok(finding(category, name.clone()));
    }
    Ok(finding(Category::Custom, "match"))
}

fn lakera_detector(ty: &str) -> Option<Category> {
    if ty == "prompt_attack" {
        return Some(Category::PromptInjection);
    }
    if ty == "unknown_links" {
        return Some(Category::Custom);
    }
    if ty.starts_with("pii/") {
        return Some(Category::Pii);
    }
    Some(match ty.strip_prefix("moderated_content/")? {
        "hate" => Category::Hate,
        "sexual" => Category::Sexual,
        "violence" | "weapons" => Category::Violence,
        "crime" => Category::Illicit,
        "profanity" => Category::Harassment,
        _ => return None,
    })
}

/// Lakera Guard: `flagged`, and the breakdown of the detectors that fired.
fn lakera(v: &Value) -> Result<Said, String> {
    let flagged = v
        .get("flagged")
        .and_then(Value::as_bool)
        .ok_or_else(|| "the answer has no boolean flagged".to_owned())?;
    if !flagged {
        return Ok(Said::Pass);
    }
    for d in v.get("breakdown").and_then(Value::as_array).into_iter().flatten() {
        if d.get("detected").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let ty = text(d, "detector_type").unwrap_or_default();
        let category = lakera_detector(ty).ok_or_else(|| format!("unrecognised detector {ty:?}"))?;
        return Ok(finding(category, ty));
    }
    Ok(finding(Category::Custom, "flagged"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partner(kind: PartnerKind, b: Value) -> Partner {
        Partner::from_block(kind, &b).unwrap()
    }

    fn one(p: &Partner, v: Value, texts: usize) -> Result<Said, String> {
        p.read(&[(200, v.to_string().into_bytes())], texts)
    }

    #[test]
    fn bedrock_blocks_anonymises_and_rejects_what_it_does_not_know() {
        let p = partner(
            PartnerKind::Bedrock,
            json!({"provider": "aws", "guardrail_id": "g1", "base_url": "http://mock/"}),
        );
        let calls = p.calls(GuardStage::Input, &["a", "b"]);
        assert_eq!(calls[0].url, "http://mock/guardrail/g1/version/DRAFT/apply");
        let sent: Value = serde_json::from_slice(&calls[0].body).unwrap();
        assert_eq!(sent["source"], "INPUT");
        assert_eq!(sent["content"][1]["text"]["text"], "b");
        let blocked = json!({"action": "GUARDRAIL_INTERVENED", "assessments": [
            {"contentPolicy": {"filters": [{"type": "PROMPT_ATTACK", "action": "BLOCKED"}]}}]});
        assert!(matches!(
            one(&p, blocked, 2),
            Ok(Said::Found {
                category: Category::PromptInjection,
                ..
            })
        ));
        let anon = json!({"action": "GUARDRAIL_INTERVENED", "outputs": [{"text": "x"}, {"text": "{EMAIL}"}],
            "assessments": [{"sensitiveInformationPolicy": {"piiEntities": [{"type": "EMAIL", "action": "ANONYMIZED"}]}}]});
        assert_eq!(
            one(&p, anon.clone(), 2),
            Ok(Said::Anonymized(vec!["x".into(), "{EMAIL}".into()]))
        );
        assert!(one(&p, anon, 3).is_err(), "masked-output count mismatch");
        assert!(one(&p, json!({"action": "MAYBE"}), 1).is_err());
        assert_eq!(one(&p, json!({"action": "NONE"}), 1), Ok(Said::Pass));
        let odd = json!({"action": "GUARDRAIL_INTERVENED", "assessments": [
            {"contentPolicy": {"filters": [{"type": "HATE", "action": "QUARANTINED"}]}}]});
        assert!(one(&p, odd, 1).is_err());
    }

    #[test]
    fn content_safety_reports_the_worst_category_over_the_threshold() {
        let p = partner(
            PartnerKind::AzureContentSafety,
            json!({"endpoint": "https://cs.example/", "api_key": "k", "severity_threshold": 4}),
        );
        let calls = p.calls(GuardStage::Input, &["hello"]);
        assert_eq!(
            calls[0].url,
            "https://cs.example/contentsafety/text:analyze?api-version=2024-09-01"
        );
        assert_eq!(
            calls[0].headers[0],
            ("ocp-apim-subscription-key".to_owned(), "k".to_owned())
        );
        let v = json!({"categoriesAnalysis": [{"category": "Hate", "severity": 2}, {"category": "Violence", "severity": 6}]});
        match one(&p, v, 1).unwrap() {
            Said::Found { category, score, .. } => {
                assert_eq!(category, Category::Violence);
                assert_eq!(score, Some(6.0));
            }
            other => panic!("{other:?}"),
        }
        let low = json!({"categoriesAnalysis": [{"category": "Hate", "severity": 2}]});
        assert_eq!(one(&p, low, 1), Ok(Said::Pass));
        assert!(
            one(
                &p,
                json!({"categoriesAnalysis": [{"category": "Medical", "severity": 0}]}),
                1
            )
            .is_err()
        );
        assert!(one(&p, json!({}), 1).is_err());
    }

    #[test]
    fn model_armor_and_lakera_map_their_filters() {
        let ma = partner(
            PartnerKind::ModelArmor,
            json!({"provider": "gcp", "project": "p", "location": "us", "template": "t"}),
        );
        let calls = ma.calls(GuardStage::Output, &["x"]);
        assert_eq!(
            calls[0].url,
            "https://modelarmor.us.rep.googleapis.com/v1/projects/p/locations/us/templates/t:sanitizeModelResponse"
        );
        let hit = json!({"sanitizationResult": {"filterMatchState": "MATCH_FOUND", "filterResults": {
            "pi_and_jailbreak": {"piAndJailbreakFilterResult": {"matchState": "MATCH_FOUND"}}}}});
        assert!(matches!(
            one(&ma, hit, 1),
            Ok(Said::Found {
                category: Category::PromptInjection,
                ..
            })
        ));
        assert!(one(&ma, json!({"sanitizationResult": {}}), 1).is_err());
        let lk = partner(PartnerKind::Lakera, json!({"api_key": "k"}));
        assert_eq!(
            lk.calls(GuardStage::Input, &["x"])[0].url,
            "https://api.lakera.ai/v2/guard"
        );
        let flagged =
            json!({"flagged": true, "breakdown": [{"detector_type": "prompt_attack", "detected": true}]});
        assert!(matches!(
            one(&lk, flagged, 1),
            Ok(Said::Found {
                category: Category::PromptInjection,
                ..
            })
        ));
        assert_eq!(one(&lk, json!({"flagged": false}), 1), Ok(Said::Pass));
        assert!(one(&lk, json!({"result": "ok"}), 1).is_err());
        let odd = json!({"flagged": true, "breakdown": [{"detector_type": "astrology", "detected": true}]});
        assert!(one(&lk, odd, 1).is_err());
    }
}

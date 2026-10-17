//! Compiling `guardrails` into [`Guardrails`], and the configuration rules the schema cannot
//! express: actions per type, execution and failure policy, patterns, attachment stages and
//! explicit window mode.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::Value;

use crate::def::{
    Action, Category, Check, Decorator, Def, GuardStage, Guardrails, Moderation, PromptMode, Replacement,
    SegKind, SystemPrompt, Webhook,
};
use crate::length::LengthCheck;
use crate::limiter::Limiter;
use crate::partners::PartnerKind;
use crate::pattern::{KeywordCheck, RegexCheck};
use crate::preserve::{PiiRules, Preserve};
use crate::rewrite::Rewrite;
use crate::secrets::SecretRules;
use crate::structured::JsonSchemaCheck;
use crate::template::Templates;
use crate::tools::ToolRules;
use crate::topic::Topic;
use crate::{pii, secrets, services, transform};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_HOLDBACK: u64 = 256;

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn uint(v: Option<&Value>, key: &str, default: usize) -> usize {
    v.and_then(|b| b.get(key))
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(default)
}

fn list<T>(v: Option<&Value>, parse: impl Fn(&str) -> Option<T>, all: &[T]) -> Vec<T>
where
    T: Copy,
{
    match v.and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_str).filter_map(parse).collect(),
        None => all.to_vec(),
    }
}

/// Compiles the document's `guardrails` (secret references resolved).
pub fn compile(doc: &Value) -> Result<Guardrails, Vec<ConfigError>> {
    let mut errors = Vec::new();
    let g = compile_into(doc, &mut errors);
    if errors.is_empty() { Ok(g) } else { Err(errors) }
}

fn compile_into(doc: &Value, errors: &mut Vec<ConfigError>) -> Guardrails {
    let block = doc.get("guardrails");
    let root = DocPath::root().key("guardrails").key("definitions");
    let mut defs = BTreeMap::new();
    for (name, d) in block
        .and_then(|b| b.get("definitions"))
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if d.get("enabled").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        if let Some(def) = definition(name, d, &root.key(name), doc, errors) {
            defs.insert(name.clone(), Arc::new(def));
        }
    }
    let uses_request_schema = defs
        .values()
        .any(|d: &Arc<Def>| matches!(&d.check, Check::JsonSchema(j) if j.use_request_schema));
    Guardrails {
        uses_request_schema,
        defs,
        hash_key: block
            .and_then(|b| s(b, "hash_key"))
            .map(|k| k.as_bytes().to_vec()),
        expose_results: block
            .and_then(|b| b.get("expose_results"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        block_uninspectable: block.and_then(|b| s(b, "on_uninspectable")) == Some("block"),
    }
}

fn conflict(errors: &mut Vec<ConfigError>, at: &DocPath, message: &str) {
    errors.push(ConfigError::new(at, DetailCode::Conflict, message));
}

fn definition(
    name: &str,
    d: &Value,
    at: &DocPath,
    doc: &Value,
    errors: &mut Vec<ConfigError>,
) -> Option<Def> {
    let ty = s(d, "type")?;
    let check = match ty {
        "regex" => Check::Regex(RegexCheck::compile(d.get("regex"), at, errors)),
        "keywords" => Check::Keywords(KeywordCheck::compile(d.get("keywords"), at, errors)),
        "pii" => Check::Pii(PiiRules {
            entities: list(
                d.get("pii").and_then(|p| p.get("entities")),
                pii::Entity::parse,
                &pii::ALL,
            ),
            preserve: list(d.get("pii").and_then(|p| p.get("preserve")), Preserve::parse, &[]),
            reversible: d.pointer("/pii/reversible").and_then(Value::as_bool) == Some(true),
        }),
        "secrets" => Check::Secrets(SecretRules {
            detectors: list(
                d.get("secrets").and_then(|p| p.get("detectors")),
                secrets::Detector::parse,
                &secrets::ALL,
            ),
            min_entropy: d
                .get("secrets")
                .and_then(|p| p.get("min_entropy"))
                .and_then(Value::as_f64),
        }),
        "moderation" => Check::Moderation(moderation(d.get("moderation"), at, doc, errors)?),
        "webhook" => Check::Webhook(webhook(d.get("webhook"))?),
        "system_prompt" => {
            let b = d.get("system_prompt")?;
            Check::SystemPrompt(SystemPrompt {
                text: s(b, "text")?.to_owned(),
                mode: match s(b, "mode") {
                    Some("replace") => PromptMode::Replace,
                    Some("append") => PromptMode::Append,
                    Some("insert_if_absent") => PromptMode::InsertIfAbsent,
                    _ => PromptMode::Prepend,
                },
                role: s(b, "role").unwrap_or("system").to_owned(),
            })
        }
        "decorator" => {
            let b = d.get("decorator");
            let text = |k: &str| b.and_then(|b| s(b, k)).map(str::to_owned);
            Check::Decorator(Decorator {
                prepend: transform::decorations(b.and_then(|b| b.get("prepend"))),
                append: transform::decorations(b.and_then(|b| b.get("append"))),
                user_prefix: text("user_prefix"),
                user_suffix: text("user_suffix"),
            })
        }
        "json_schema" => Check::JsonSchema(JsonSchemaCheck::compile(d.get("json_schema"), at, errors)),
        "tool_permission" => Check::ToolPermission(ToolRules::from_block(d.get("tool_permission"))),
        "length" => Check::Length(LengthCheck::compile(d.get("length"), doc, at, errors)),
        "llm_judge" => Check::Judge(services::judge(d.get("llm_judge"), at, doc, errors)?),
        "classifier" => Check::Classifier(services::classifier(d.get("classifier"), at, errors)?),
        "template" => Check::Template(Templates::compile(d.get("template"), at, errors)),
        "llm_rewrite" => Check::Rewrite(Rewrite::compile(d.get("llm_rewrite"), at, doc, errors)?),
        "json_repair" => Check::JsonRepair,
        "topic" => Check::Topic(Topic::compile(d.get("topic"), at, doc, errors)?),
        other => {
            let kind = PartnerKind::parse(other)?;
            Check::Partner(services::partner(kind, d.get(other), at, doc, errors)?)
        }
    };
    let default_action = match ty {
        "pii" | "secrets" | "llm_rewrite" => Action::Redact,
        _ => Action::Block,
    };
    let action = match s(d, "action") {
        Some("block") => Action::Block,
        Some("redact") => Action::Redact,
        Some("flag") => Action::Flag,
        Some("divert") => Action::Divert,
        _ => default_action,
    };
    let transform = matches!(
        check,
        Check::SystemPrompt(_) | Check::Decorator(_) | Check::Template(_) | Check::JsonRepair
    );
    if action == Action::Redact
        && !matches!(
            ty,
            "regex"
                | "keywords"
                | "pii"
                | "secrets"
                | "webhook"
                | "tool_permission"
                | "bedrock_guardrails"
                | "llm_rewrite"
        )
    {
        conflict(errors, &at.key("action"), "this guardrail type cannot redact");
    }
    if matches!(check, Check::Rewrite(_)) && action != Action::Redact {
        conflict(
            errors,
            &at.key("action"),
            "an llm_rewrite guardrail can only redact",
        );
    }
    let divert_to = s(d, "divert_to").map(str::to_owned);
    if action == Action::Divert {
        if transform || matches!(check, Check::JsonSchema(_) | Check::Rewrite(_)) {
            conflict(errors, &at.key("action"), "only an input check can divert");
        }
        match &divert_to {
            Some(m) if doc.get("models").and_then(|ms| ms.get(m)).is_none() => {
                errors.push(ConfigError::new(
                    &at.key("divert_to"),
                    DetailCode::UnresolvedReference,
                    format!("model {m:?} does not exist"),
                ));
            }
            Some(_) => {}
            None => errors.push(ConfigError::new(
                &at.key("divert_to"),
                DetailCode::Required,
                "action divert needs divert_to",
            )),
        }
    }
    if let Check::Pii(p) = &check
        && p.reversible
    {
        if action != Action::Redact {
            conflict(
                errors,
                &at.key("pii").key("reversible"),
                "reversible tokens need action redact",
            );
        }
        if !p.preserve.is_empty() {
            conflict(
                errors,
                &at.key("pii").key("preserve"),
                "a reversible token replaces the whole value",
            );
        }
        if s(d, "replacement").is_some_and(|r| r != "label") {
            conflict(
                errors,
                &at.key("replacement"),
                "a reversible token replaces the whole value",
            );
        }
    }
    let asynchronous = s(d, "execution") == Some("async");
    let parallel = s(d, "execution") == Some("parallel_with_call");
    if parallel && (transform || matches!(action, Action::Redact | Action::Divert)) {
        conflict(
            errors,
            &at.key("execution"),
            "execution parallel_with_call requires action block or flag",
        );
    }
    if asynchronous && action != Action::Flag && !transform {
        conflict(
            errors,
            &at.key("execution"),
            "execution async requires action flag",
        );
    }
    let on_error = s(d, "on_error");
    if on_error == Some("block") && (action == Action::Flag || asynchronous) {
        conflict(
            errors,
            &at.key("on_error"),
            "a flag or async guardrail cannot fail closed",
        );
    }
    let fail_open = match on_error {
        Some("allow") => true,
        Some("block") => false,
        _ => action == Action::Flag || asynchronous,
    };
    if let Check::Regex(r) = &check
        && r.has_allow()
        && action == Action::Redact
    {
        conflict(
            errors,
            &at.key("regex").key("allow"),
            "allow patterns require action block or flag",
        );
    }
    let scope = d.get("scope");
    let kinds = scope
        .and_then(|sc| sc.get("kinds"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter_map(SegKind::parse_input)
                .collect()
        });
    let mut categories = BTreeMap::new();
    for (label, c) in d
        .get("categories")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        match c.as_str().and_then(Category::parse) {
            Some(cat) => {
                categories.insert(label.clone(), cat);
            }
            None => errors.push(ConfigError::new(
                &at.key("categories").key(label),
                DetailCode::InvalidEnum,
                "not a guardrail category",
            )),
        }
    }
    let networked = matches!(
        check,
        Check::Moderation(_)
            | Check::Webhook(_)
            | Check::Judge(_)
            | Check::Classifier(_)
            | Check::Partner(_)
            | Check::Rewrite(_)
            | Check::Topic(_)
    );
    let limiter = d
        .get("max_concurrency")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|_| networked)
        .map(Limiter::new);
    let stages: Vec<GuardStage> = d
        .get("request_stages")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| match v.as_str() {
                    Some("input") => Some(GuardStage::Input),
                    Some("output") => Some(GuardStage::Output),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let severity = s(d, "severity")
        .unwrap_or(match action {
            Action::Block => "high",
            Action::Redact => "medium",
            Action::Flag | Action::Divert => "low",
        })
        .to_owned();
    let notify_min = d
        .get("notify")
        .and_then(|n| s(n, "min_severity"))
        .unwrap_or("high");
    let mut def = Def {
        name: name.to_owned(),
        check,
        action,
        asynchronous,
        parallel,
        fail_open,
        order: d.get("order").and_then(Value::as_i64).unwrap_or(0),
        message: s(d, "message").map(str::to_owned),
        include_operator: d.get("include_operator_content").and_then(Value::as_bool) == Some(true),
        kinds,
        last_user: scope.and_then(|sc| s(sc, "messages")) == Some("last_user"),
        replacement: match s(d, "replacement") {
            Some("mask") => Replacement::Mask,
            Some("hash") => Replacement::Hash,
            _ => Replacement::Label,
        },
        timeout: s(d, "timeout")
            .and_then(antirion_config::scalar::parse_duration)
            .unwrap_or(DEFAULT_TIMEOUT),
        categories,
        notify: crate::def::severity_rank(&severity) >= crate::def::severity_rank(notify_min),
        severity,
        limiter,
        request_stages: Vec::new(),
        divert_to,
    };
    let supported = def.stages();
    def.request_stages = if stages.is_empty() {
        supported.to_vec()
    } else {
        for (i, st) in stages.iter().enumerate() {
            if !supported.contains(st) {
                conflict(
                    errors,
                    &at.key("request_stages").index(i),
                    "this guardrail cannot run at that stage",
                );
            }
        }
        stages
    };
    Some(def)
}

fn moderation(
    b: Option<&Value>,
    at: &DocPath,
    doc: &Value,
    errors: &mut Vec<ConfigError>,
) -> Option<Moderation> {
    let model = b.and_then(|b| s(b, "model"))?.to_owned();
    if doc.get("models").and_then(|m| m.get(&model)).is_none() {
        errors.push(ConfigError::new(
            &at.key("moderation").key("model"),
            DetailCode::UnresolvedReference,
            format!("model {model:?} does not exist"),
        ));
    }
    let max_chars = uint(b, "max_chars", 8000);
    let overlap = uint(b, "overlap_chars", 200);
    if overlap >= max_chars {
        errors.push(ConfigError::new(
            &at.key("moderation").key("overlap_chars"),
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
    Some(Moderation {
        model,
        thresholds,
        max_inputs_per_call: uint(b, "max_inputs_per_call", 32).max(1),
        max_chars: max_chars.max(1),
        overlap_chars: overlap.min(max_chars.saturating_sub(1)),
    })
}

fn webhook(b: Option<&Value>) -> Option<Webhook> {
    let b = b?;
    let headers = b
        .get("headers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
        .collect();
    Some(Webhook {
        url: s(b, "url")?.to_owned(),
        headers,
        signing_secret: s(b, "signing_secret").map(antirion_core::Secret::new),
        body_mode: s(b, "mode") == Some("body"),
    })
}

/// Every configuration error of the document's guardrails: definitions, the stage of every
/// attachment, and explicit window mode with patterns longer than the holdback.
#[must_use]
pub fn check_document(doc: &Value) -> Vec<ConfigError> {
    let mut errors = Vec::new();
    let g = compile_into(doc, &mut errors);
    for (path, block) in antirion_config::semantic::policy_blocks(doc) {
        let Some(att) = block.get("guardrails") else {
            continue;
        };
        for list in ["input", "output", "mcp_input", "mcp_output"] {
            for (i, name) in att
                .get(list)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                let Some(d) = name.as_str().and_then(|n| g.get(n)) else {
                    continue;
                };
                if let Some(why) = misplaced(d, list) {
                    conflict(&mut errors, &path.key("guardrails").key(list).index(i), why);
                }
            }
        }
        window_rule(doc, &g, att, &mut errors);
    }
    errors
}

/// Why a guardrail cannot be attached to `list`, if it cannot.
fn misplaced(d: &Def, list: &str) -> Option<&'static str> {
    let output = list.ends_with("output");
    if list.starts_with("mcp")
        && (d.is_transform()
            || d.reversible()
            || d.action == Action::Divert
            || matches!(d.check, Check::Rewrite(_) | Check::Topic(_)))
    {
        return Some("MCP traffic takes only checks of text");
    }
    if matches!(d.check, Check::JsonRepair) {
        return (list != "output").then_some("json_repair runs only at the output stage");
    }
    if list != "input" && d.is_transform() {
        return Some("a transform runs only at the input stage");
    }
    if output && !d.stages().contains(&GuardStage::Output) {
        return Some("this guardrail runs only at the input stage");
    }
    if list == "input" && matches!(d.check, Check::JsonSchema(_)) {
        return Some("a json_schema guardrail checks responses only");
    }
    if output && d.parallel {
        return Some("execution parallel_with_call runs only at the input stage");
    }
    if output && d.action == Action::Redact && matches!(d.check, Check::ToolPermission(_)) {
        return Some("at the output stage a tool_permission guardrail can only block or flag");
    }
    None
}

/// Explicit `streaming.mode: window` rejects an enforcing pattern longer than the holdback.
fn window_rule(doc: &Value, g: &Guardrails, att: &Value, errors: &mut Vec<ConfigError>) {
    let streaming = att.get("streaming");
    if streaming.and_then(|st| s(st, "mode")) != Some("window") {
        return;
    }
    let holdback = streaming
        .and_then(|st| st.get("holdback_chars"))
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_HOLDBACK);
    let limit = usize::try_from(holdback).unwrap_or(usize::MAX);
    let gateway = doc
        .pointer("/policies/guardrails/output")
        .and_then(Value::as_array);
    let names = att
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(gateway.into_iter().flatten())
        .filter_map(Value::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let defs = DocPath::root().key("guardrails").key("definitions");
    for name in names {
        let Some(d) = g.get(name) else { continue };
        if !d.enforcing() || d.window_len().is_some_and(|l| l <= limit) {
            continue;
        }
        let raw = doc.pointer(&format!("/guardrails/definitions/{name}"));
        let at = defs.key(name);
        let paths: Vec<DocPath> = match &d.check {
            Check::Regex(r) => r
                .longer_than(raw.and_then(|r| r.get("regex")), limit)
                .into_iter()
                .map(|i| at.key("regex").key("deny").index(i))
                .collect(),
            Check::Keywords(_) => vec![at.key("keywords").key("words")],
            Check::Pii(_) => vec![at.key("pii")],
            Check::Secrets(_) => vec![at.key("secrets")],
            _ => Vec::new(),
        };
        for p in paths {
            errors.push(ConfigError::new(
                &p,
                DetailCode::Conflict,
                format!("this pattern can match more than holdback_chars ({holdback}) characters, so window mode cannot enforce it"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn paths(doc: &Value) -> Vec<String> {
        check_document(doc).into_iter().map(|e| e.path).collect()
    }

    #[test]
    fn execution_and_failure_rules() {
        let doc = json!({"guardrails": {"definitions": {
            "a": {"type": "webhook", "execution": "async", "webhook": {"url": "https://x"}},
            "b": {"type": "keywords", "action": "flag", "on_error": "block", "keywords": {"words": ["x"]}},
            "c": {"type": "regex", "action": "redact", "regex": {"allow": ["ok"]}},
            "d": {"type": "moderation", "action": "redact", "moderation": {"model": "m"}},
            "e": {"type": "regex", "action": "divert", "regex": {"deny": ["x"]}}
        }}, "models": {"m": {}}});
        assert_eq!(
            paths(&doc),
            [
                "guardrails.definitions.a.execution",
                "guardrails.definitions.b.on_error",
                "guardrails.definitions.c.regex.allow",
                "guardrails.definitions.d.action",
                "guardrails.definitions.e.divert_to",
            ]
        );
    }

    #[test]
    fn transforms_attach_only_to_input_and_window_rejects_long_patterns() {
        let doc = json!({
            "guardrails": {"definitions": {
                "sp": {"type": "system_prompt", "system_prompt": {"text": "x", "mode": "prepend"}},
                "g": {"type": "regex", "regex": {"deny": ["ok", "a.*b"]}}}},
            "policies": {"guardrails": {"output": ["sp", "g"], "streaming": {"mode": "window"}}}
        });
        assert_eq!(
            paths(&doc),
            [
                "policies.guardrails.output[0]",
                "guardrails.definitions.g.regex.deny[1]"
            ]
        );
    }

    #[test]
    fn defaults_follow_the_type() {
        let doc = json!({"guardrails": {"definitions": {
            "p": {"type": "pii"}, "w": {"type": "webhook", "action": "flag", "webhook": {"url": "https://x"}}}}});
        let g = compile(&doc).unwrap();
        assert_eq!(g.defs["p"].action, Action::Redact);
        assert!(!g.defs["p"].fail_open);
        assert!(g.defs["w"].fail_open);
        assert!(g.expose_results);
    }
}

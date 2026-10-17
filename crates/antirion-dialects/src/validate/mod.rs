//! Request-body schema checks run at `parse`: each endpoint's validation table, applied to the
//! members a client sent, so that a violation names the first offending field before any upstream
//! call. `model` is left to the caller, which checks it after these rules.

mod chat;
mod endpoints;
mod rerank;

use antirion_json::Skim;
use serde_json::Value;

use crate::endpoints::Endpoint;

/// Which rule a field broke.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ViolationKind {
    /// A required field is absent.
    Missing,
    /// The field has the wrong JSON type.
    Type,
    /// The field has the right type but a value outside its rule.
    Value,
}

/// The first violation found in a body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// Dotted path with indexes: `messages[1].role`, `input[3]`, `tools[0].function.name`.
    pub path: String,
    pub kind: ViolationKind,
    /// A fixed sentence that starts with `path`.
    pub message: String,
}

/// Settings the validation tables depend on.
#[derive(Clone, Debug)]
pub struct ValidateOpts {
    /// Upper bound of `n` on image generation.
    pub images_max_n: u64,
    /// A top-level `template` object may stand in for absent or empty `messages` (`input`), which
    /// a template rendered later supplies.
    pub templated: bool,
}

impl Default for ValidateOpts {
    fn default() -> ValidateOpts {
        ValidateOpts {
            images_max_n: 10,
            templated: false,
        }
    }
}

/// The members a request's template supplies.
const TEMPLATED: &[&str] = &["messages", "input"];

fn empty(v: &Value) -> bool {
    match v {
        Value::Array(a) => a.is_empty(),
        Value::String(s) => s.is_empty(),
        Value::Null => true,
        _ => false,
    }
}

/// The OpenAI `error.code` of a violation.
#[must_use]
pub fn openai_code(kind: ViolationKind) -> &'static str {
    match kind {
        ViolationKind::Missing => "missing_required_parameter",
        ViolationKind::Type => "invalid_type",
        ViolationKind::Value => "invalid_value",
    }
}

/// The reason of a violation; equal to its OpenAI code.
#[must_use]
pub fn reason(kind: ViolationKind) -> &'static str {
    openai_code(kind)
}

pub(crate) type Check = Result<(), Violation>;

/// Rewrites a table's violation with what the whole body shows (raw bytes and members).
pub(crate) type ErrorHook = fn(Violation, &[u8], &Skim) -> Violation;

/// One endpoint's table: a check per present top-level member, the members that must be present
/// (reported in this order after every present member passed), and an optional whole-body rule.
pub(crate) struct Table {
    pub(crate) member: fn(&str, &Value, &ValidateOpts) -> Check,
    pub(crate) required: &'static [&'static str],
    pub(crate) finish: Option<fn(&Skim) -> Check>,
    pub(crate) on_error: Option<ErrorHook>,
}

fn table(endpoint: &Endpoint) -> Option<&'static Table> {
    match endpoint.id {
        "chat_completions" => Some(&chat::CHAT),
        "completions" => Some(&endpoints::COMPLETIONS),
        "embeddings" => Some(&endpoints::EMBEDDINGS),
        "images" => Some(&endpoints::IMAGES),
        "audio" if endpoint.upstream_path == Some("/audio/speech") => Some(&endpoints::SPEECH),
        "responses" => Some(&endpoints::RESPONSES),
        "moderations" => Some(&endpoints::MODERATIONS),
        "messages" => Some(&endpoints::MESSAGES),
        "count_tokens" => Some(&endpoints::COUNT_TOKENS),
        "rerank" => Some(&rerank::RERANK),
        _ => None,
    }
}

/// Validates a JSON object body for its endpoint and returns the first violation in document order:
/// present top-level members in the order they appear in `raw`, values inside them in their natural
/// order, then absent required members in the table's order. Unknown members are never violations.
pub fn validate_json(
    endpoint: &Endpoint,
    raw: &[u8],
    skim: &Skim,
    doc: Option<&Value>,
    opts: &ValidateOpts,
) -> Result<(), Violation> {
    let Some(t) = table(endpoint) else { return Ok(()) };
    let result = run(t, raw, skim, doc, opts);
    match (result, t.on_error) {
        (Err(v), Some(hook)) => Err(hook(v, raw, skim)),
        (r, _) => r,
    }
}

fn run(t: &Table, raw: &[u8], skim: &Skim, doc: Option<&Value>, opts: &ValidateOpts) -> Check {
    let templated = opts.templated && skim.member("template").is_some();
    for m in &skim.members {
        if m.name == "model" {
            continue;
        }
        let parsed;
        let value = match doc.and_then(|d| d.get(m.name.as_str())) {
            Some(v) => v,
            None => match member_value(raw, &m.value) {
                Some(v) => {
                    parsed = v;
                    &parsed
                }
                None => continue,
            },
        };
        if templated && TEMPLATED.contains(&m.name.as_str()) && empty(value) {
            continue;
        }
        (t.member)(&m.name, value, opts)?;
    }
    for name in t.required {
        if skim.member(name).is_none() && !(templated && TEMPLATED.contains(name)) {
            return Err(missing(name));
        }
    }
    match t.finish {
        Some(_) if templated && TEMPLATED.iter().all(|n| skim.member(n).is_none()) => Ok(()),
        Some(f) => f(skim),
        None => Ok(()),
    }
}

pub(crate) fn member_value(raw: &[u8], span: &std::ops::Range<usize>) -> Option<Value> {
    serde_json::from_slice(raw.get(span.clone())?).ok()
}

pub(crate) fn violation(path: &str, kind: ViolationKind, detail: &str) -> Violation {
    Violation {
        path: path.to_owned(),
        kind,
        message: format!("{path}: {detail}"),
    }
}

pub(crate) fn missing(path: &str) -> Violation {
    violation(path, ViolationKind::Missing, "is required.")
}

pub(crate) fn wrong_type(path: &str, expected: &str) -> Violation {
    violation(path, ViolationKind::Type, &format!("must be {expected}."))
}

pub(crate) fn bad_value(path: &str, detail: &str) -> Violation {
    violation(path, ViolationKind::Value, detail)
}

pub(crate) fn key(parent: &str, name: &str) -> String {
    format!("{parent}.{name}")
}

pub(crate) fn index(parent: &str, i: usize) -> String {
    format!("{parent}[{i}]")
}

/// A number in `[lo, hi]`. Booleans are not numbers.
pub(crate) fn number_in(v: &Value, path: &str, lo: f64, hi: f64) -> Check {
    let Some(n) = v.as_f64() else {
        return Err(wrong_type(path, "a number"));
    };
    if n < lo || n > hi {
        return Err(bad_value(path, &format!("must be between {lo} and {hi}.")));
    }
    Ok(())
}

/// An integer of at least `lo` and at most `hi` when given. A number with a fraction or an
/// exponent is not an integer.
pub(crate) fn integer_in(v: &Value, path: &str, lo: i64, hi: Option<i64>) -> Check {
    let out_of_range = || match hi {
        Some(h) => bad_value(path, &format!("must be an integer between {lo} and {h}.")),
        None => bad_value(path, &format!("must be an integer of at least {lo}.")),
    };
    if let Some(i) = v.as_i64() {
        if i < lo || hi.is_some_and(|h| i > h) {
            return Err(out_of_range());
        }
        return Ok(());
    }
    if v.as_u64().is_some() {
        return if hi.is_some() { Err(out_of_range()) } else { Ok(()) };
    }
    Err(wrong_type(path, "an integer"))
}

pub(crate) fn string<'a>(v: &'a Value, path: &str) -> Result<&'a str, Violation> {
    v.as_str().ok_or_else(|| wrong_type(path, "a string"))
}

pub(crate) fn non_empty_string(v: &Value, path: &str) -> Check {
    if string(v, path)?.is_empty() {
        return Err(bad_value(path, "must not be empty."));
    }
    Ok(())
}

pub(crate) fn boolean(v: &Value, path: &str) -> Check {
    if v.is_boolean() {
        Ok(())
    } else {
        Err(wrong_type(path, "a boolean"))
    }
}

pub(crate) fn object<'a>(v: &'a Value, path: &str) -> Result<&'a serde_json::Map<String, Value>, Violation> {
    v.as_object().ok_or_else(|| wrong_type(path, "an object"))
}

pub(crate) fn array<'a>(v: &'a Value, path: &str) -> Result<&'a Vec<Value>, Violation> {
    v.as_array().ok_or_else(|| wrong_type(path, "an array"))
}

#[cfg(test)]
pub(crate) mod tests {
    use antirion_json::{JsonLimits, skim};
    use http::Method;

    use super::*;
    use crate::endpoints::{Matched, match_endpoint};

    pub(crate) fn check_at(path: &str, body: &str) -> Result<(), Violation> {
        let endpoint = match match_endpoint(&Method::POST, path) {
            Matched::Found(e) => e,
            other => panic!("{path}: {other:?}"),
        };
        let raw = body.as_bytes();
        let s = skim(raw, &JsonLimits::default()).unwrap();
        let doc: Option<Value> = serde_json::from_slice(raw).ok();
        let checked = validate_json(&endpoint, raw, &s, doc.as_ref(), &ValidateOpts::default());
        assert_eq!(
            checked,
            validate_json(&endpoint, raw, &s, None, &ValidateOpts::default())
        );
        checked
    }

    /// Asserts the violation's path and kind (`None` leaves the kind open).
    pub(crate) fn rejects(path: &str, body: &str, param: &str, kind: Option<ViolationKind>) {
        let v = check_at(path, body).expect_err(body);
        assert_eq!(v.path, param, "{body}: {}", v.message);
        if let Some(k) = kind {
            assert_eq!(v.kind, k, "{body}: {}", v.message);
        }
        assert!(v.message.starts_with(param), "{}", v.message);
    }

    #[test]
    fn codes() {
        assert_eq!(openai_code(ViolationKind::Missing), "missing_required_parameter");
        assert_eq!(openai_code(ViolationKind::Type), "invalid_type");
        assert_eq!(reason(ViolationKind::Value), "invalid_value");
    }

    #[test]
    fn integers() {
        assert!(integer_in(&serde_json::json!(1), "n", 1, Some(2)).is_ok());
        assert_eq!(
            integer_in(&serde_json::json!(1.5), "n", 1, Some(2))
                .unwrap_err()
                .kind,
            ViolationKind::Type
        );
        assert_eq!(
            integer_in(&serde_json::json!(2.0), "n", 1, Some(2))
                .unwrap_err()
                .kind,
            ViolationKind::Type
        );
        assert_eq!(
            integer_in(&serde_json::json!(u64::MAX), "n", 1, Some(2))
                .unwrap_err()
                .kind,
            ViolationKind::Value
        );
        assert!(integer_in(&serde_json::json!(u64::MAX), "n", 1, None).is_ok());
        assert_eq!(
            integer_in(&serde_json::json!(true), "n", 1, None)
                .unwrap_err()
                .kind,
            ViolationKind::Type
        );
    }

    #[test]
    fn endpoints_without_a_table_accept_anything() {
        assert!(check_at("/v1/audio/transcriptions", "{}").is_ok());
        assert!(check_at("/v1/messages/count_tokens", "{}").is_err());
        assert!(
            check_at(
                "/v1/messages/count_tokens",
                r#"{"messages":[{"role":"user","content":"x"}]}"#
            )
            .is_ok()
        );
    }

    #[test]
    fn a_template_object_stands_in_for_messages_only_when_allowed() {
        let Matched::Found(e) = match_endpoint(&Method::POST, "/v1/chat/completions") else {
            panic!("chat completions is an endpoint");
        };
        let check = |body: &str, templated: bool| {
            let s = skim(body.as_bytes(), &JsonLimits::default()).unwrap();
            let opts = ValidateOpts {
                templated,
                ..ValidateOpts::default()
            };
            validate_json(&e, body.as_bytes(), &s, None, &opts)
        };
        let templated = r#"{"model":"m","template":{"name":"t"}}"#;
        assert!(check(templated, true).is_ok());
        assert_eq!(check(templated, false).unwrap_err().path, "messages");
        assert!(check(r#"{"model":"m","template":{},"messages":[]}"#, true).is_ok());
        assert!(check(r#"{"model":"m","messages":[]}"#, true).is_err());
    }
}

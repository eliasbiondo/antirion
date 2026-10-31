//! Typed decisions: `state` and a named `questions` object, answered once with the whole answer.

use serde_json::Value;

use super::{Check, Table, ValidateOpts, bad_value, boolean, key, object, wrong_type};

pub(crate) static SYSTEMONE: Table = Table {
    member: systemone,
    required: &["state", "questions"],
    finish: None,
    on_error: None,
};

fn systemone(name: &str, v: &Value, _opts: &ValidateOpts) -> Check {
    match name {
        // A decision has no stream shape: accepting the field would promise an answer the endpoint
        // cannot give. `stream: false` states what happens anyway, so it is not a refusal.
        "stream" if !v.is_null() => {
            boolean(v, name)?;
            if v.as_bool() == Some(true) {
                return Err(bad_value(name, "is not supported on this endpoint."));
            }
            Ok(())
        }
        "questions" => questions(v),
        _ => Ok(()),
    }
}

/// One question per entry, at least one; each question is its own object.
fn questions(v: &Value) -> Check {
    let entries = object(v, "questions")?;
    if entries.is_empty() {
        return Err(bad_value("questions", "must have at least one entry."));
    }
    for (name, question) in entries {
        if !question.is_object() {
            return Err(wrong_type(&key("questions", name), "an object"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use antirion_json::{JsonLimits, skim};
    use http::Method;

    use crate::{Matched, ValidateOpts, ViolationKind, match_endpoint, validate_json};

    fn check(body: &str) -> Result<(), (String, ViolationKind)> {
        let Matched::Found(e) = match_endpoint(&Method::POST, "/v1/systemone") else {
            panic!("systemone is in the catalog");
        };
        let raw = body.as_bytes();
        let s = skim(raw, &JsonLimits::default()).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(raw).unwrap();
        validate_json(&e, raw, &s, Some(&doc), &ValidateOpts::default()).map_err(|v| (v.path, v.kind))
    }

    #[test]
    fn systemone_bodies() {
        let q = r#""questions":{"q":{"type":"choice","criteria":{"a":"A","b":"B"}}}"#;
        assert!(check(&format!(r#"{{"model":"m","state":"s",{q}}}"#)).is_ok());
        assert!(check(r#"{"model":"m","state":null,"questions":{"q":{"type":"noul"}}}"#).is_ok());
        assert!(check(r#"{"model":"m","state":"s","questions":{"q":{}},"stream":false}"#).is_ok());
        assert_eq!(
            check(r#"{"model":"m","state":"s","questions":{"q":{}},"stream":true}"#),
            Err(("stream".into(), ViolationKind::Value))
        );
        assert_eq!(
            check(r#"{"model":"m","state":"s","questions":{"q":{}},"stream":"yes"}"#),
            Err(("stream".into(), ViolationKind::Type))
        );
        assert_eq!(
            check(r#"{"model":"m","state":"s"}"#),
            Err(("questions".into(), ViolationKind::Missing))
        );
        assert_eq!(
            check(r#"{"model":"m","questions":{"q":{}}}"#),
            Err(("state".into(), ViolationKind::Missing))
        );
        assert_eq!(
            check(r#"{"model":"m","state":"s","questions":{}}"#),
            Err(("questions".into(), ViolationKind::Value))
        );
        assert_eq!(
            check(r#"{"model":"m","state":"s","questions":[]}"#),
            Err(("questions".into(), ViolationKind::Type))
        );
        assert_eq!(
            check(r#"{"model":"m","state":"s","questions":{"q":"choice"}}"#),
            Err(("questions.q".into(), ViolationKind::Type))
        );
    }
}

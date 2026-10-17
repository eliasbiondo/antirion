//! Rerank in the Cohere shape: `query`, `documents`, `top_n` and `return_documents`.

use serde_json::Value;

use super::{
    Check, Table, ValidateOpts, array, bad_value, boolean, index, integer_in, non_empty_string, wrong_type,
};

pub(crate) static RERANK: Table = Table {
    member: rerank,
    required: &["query", "documents"],
    finish: None,
    on_error: None,
};

fn rerank(name: &str, v: &Value, _opts: &ValidateOpts) -> Check {
    match name {
        "query" => non_empty_string(v, name),
        "documents" => documents(v),
        "top_n" if !v.is_null() => integer_in(v, name, 1, None),
        "return_documents" if !v.is_null() => boolean(v, name),
        _ => Ok(()),
    }
}

/// A non-empty array whose items are strings or objects.
fn documents(v: &Value) -> Check {
    let items = array(v, "documents")?;
    if items.is_empty() {
        return Err(bad_value("documents", "must not be empty."));
    }
    for (i, item) in items.iter().enumerate() {
        if !matches!(item, Value::String(_) | Value::Object(_)) {
            return Err(wrong_type(&index("documents", i), "a string or an object"));
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
        let Matched::Found(e) = match_endpoint(&Method::POST, "/v1/rerank") else {
            panic!("rerank is in the catalog");
        };
        let raw = body.as_bytes();
        let s = skim(raw, &JsonLimits::default()).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(raw).unwrap();
        validate_json(&e, raw, &s, Some(&doc), &ValidateOpts::default()).map_err(|v| (v.path, v.kind))
    }

    #[test]
    fn rerank_bodies() {
        assert!(check(r#"{"model":"m","query":"q","documents":["a",{"text":"b"}],"top_n":1}"#).is_ok());
        assert_eq!(
            check(r#"{"model":"m","query":"q","documents":[]}"#),
            Err(("documents".into(), ViolationKind::Value))
        );
        assert_eq!(
            check(r#"{"model":"m","query":"","documents":["a"]}"#),
            Err(("query".into(), ViolationKind::Value))
        );
        assert_eq!(
            check(r#"{"model":"m","query":"q","documents":["a",3]}"#),
            Err(("documents[1]".into(), ViolationKind::Type))
        );
        assert_eq!(
            check(r#"{"model":"m","query":"q","documents":["a"],"top_n":0}"#),
            Err(("top_n".into(), ViolationKind::Value))
        );
        assert_eq!(
            check(r#"{"model":"m","documents":["a"]}"#),
            Err(("query".into(), ViolationKind::Missing))
        );
    }
}

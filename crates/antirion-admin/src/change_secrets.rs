//! The literal secrets a change request's writes carry: redacted wherever the request is read,
//! kept sealed at rest for the apply, and dropped once the request is closed.

use antirion_config::{DocPath, model, schema, secrets};
use antirion_runtime::atrest::Sealer;
use serde_json::{Map, Value, json};

use crate::error::AdminError;

/// Where a change's `body.spec` lands in the document.
enum Place {
    Setting(&'static model::SingletonDef),
    Resource(&'static model::CollectionDef, String),
}

fn place(change: &Value) -> Option<Place> {
    let path = change.get("path")?.as_str()?;
    let rel = path.strip_prefix("/admin/v1").unwrap_or(path);
    let rel = rel.split('?').next().unwrap_or_default();
    let segs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    let body_name = || {
        change
            .pointer("/body/name")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    match segs.as_slice() {
        ["settings", key] => model::singleton(key).map(Place::Setting),
        [c] => model::collection(c)
            .zip(body_name())
            .map(|(d, n)| Place::Resource(d, n)),
        [a, b] => match model::collection(&format!("{a}/{b}")) {
            Some(d) => body_name().map(|n| Place::Resource(d, n)),
            None => model::collection(a).map(|d| Place::Resource(d, (*b).to_owned())),
        },
        [a, b, n] => model::collection(&format!("{a}/{b}")).map(|d| Place::Resource(d, (*n).to_owned())),
        _ => None,
    }
}

/// A document holding only `spec` at `place`.
fn alone(place: &Place, spec: &Value) -> Value {
    let mut doc = Value::Object(Map::new());
    match place {
        Place::Setting(def) => {
            if let Some(root) = doc.as_object_mut() {
                root.insert(def.name.to_owned(), spec.clone());
            }
        }
        Place::Resource(def, name) => {
            if let Some(map) = model::collection_map_mut(&mut doc, def) {
                map.insert(name.clone(), spec.clone());
            }
        }
    }
    doc
}

fn spec_at(place: &Place, doc: &Value) -> Option<Value> {
    match place {
        Place::Setting(def) => doc.get(def.name).cloned(),
        Place::Resource(def, name) => model::collection_map(doc, def).and_then(|m| m.get(name)).cloned(),
    }
}

/// `change` with every literal secret of its body replaced by the redaction marker: the secret
/// fields the schema finds in the body where it lands, and those of `known` (the staged
/// document's, which also name fields a merge patch only reaches through what is stored).
fn redact_change(change: &Value, known: &[DocPath]) -> Value {
    let Some(spec) = change.pointer("/body/spec").filter(|s| s.is_object()) else {
        return change.clone();
    };
    let Some(place) = place(change) else {
        return change.clone();
    };
    let doc = alone(&place, spec);
    let mut paths = schema::check_document(&doc).secret_paths;
    paths.extend_from_slice(known);
    let redacted = secrets::redact(&doc, &paths);
    let mut out = change.clone();
    if let (Some(slot), Some(spec)) = (out.pointer_mut("/body/spec"), spec_at(&place, &redacted)) {
        *slot = spec;
    }
    out
}

/// The change set as readers see it.
pub(crate) fn redact_changes(list: &[Value], known: &[DocPath]) -> Vec<Value> {
    list.iter().map(|c| redact_change(c, known)).collect()
}

fn location(id: &str) -> String {
    format!("change_requests/{id}/changes")
}

/// The member a request keeps its sealed change set in, when redaction changed anything.
pub(crate) const SEALED: &str = "_sealed_changes";

/// Seals `raw` into `body` when the shown change set differs from it.
pub(crate) fn keep(
    sealer: &Sealer,
    body: &mut Value,
    id: &str,
    raw: &[Value],
    shown: &[Value],
) -> Result<(), AdminError> {
    if raw == shown {
        return Ok(());
    }
    let plain = serde_json::to_vec(raw).map_err(|_| AdminError::internal())?;
    let sealed = sealer
        .seal(&location(id), &plain)
        .map_err(|_| AdminError::internal())?;
    body[SEALED] = json!(sealed);
    Ok(())
}

/// The change set to apply: the sealed one when the request keeps one, else the stored one.
pub(crate) fn literal(sealer: &Sealer, body: &Value) -> Result<Vec<Value>, AdminError> {
    let id = body.get("id").and_then(Value::as_str).unwrap_or_default();
    match body.get(SEALED).and_then(Value::as_str) {
        Some(sealed) => {
            let plain = sealer
                .open(&location(id), sealed)
                .map_err(|_| AdminError::internal())?;
            serde_json::from_slice(&plain).map_err(|_| AdminError::internal())
        }
        None => Ok(body["changes"].as_array().cloned().unwrap_or_default()),
    }
}

/// Drops the sealed change set of a request that can no longer be applied.
pub(crate) fn forget(body: &mut Value) -> bool {
    body.as_object_mut().is_some_and(|o| o.remove(SEALED).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealer() -> Sealer {
        Sealer::new(&[vec![7u8; 32]]).unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn a_literal_secret_in_a_settings_patch_is_redacted() {
        let change = json!({"method": "PATCH", "path": "/settings/smtp",
            "body": {"spec": {"host": "mail.example", "password": "hunter2"}}});
        let shown = redact_change(&change, &[]);
        assert_eq!(
            shown.pointer("/body/spec/password"),
            Some(&json!(secrets::REDACTED))
        );
        assert_eq!(shown.pointer("/body/spec/host"), Some(&json!("mail.example")));
    }

    #[test]
    fn a_resource_create_is_redacted_where_it_lands() {
        let change = json!({"method": "POST", "path": "/providers",
            "body": {"name": "p", "spec": {"type": "openai", "auth": {"api_key": "sk-live-1"}}}});
        let shown = redact_change(&change, &[]);
        assert_eq!(
            shown.pointer("/body/spec/auth/api_key"),
            Some(&json!(secrets::REDACTED))
        );
        assert_eq!(shown.pointer("/body/name"), Some(&json!("p")));
        let reference = json!({"method": "PATCH", "path": "/providers/p",
            "body": {"spec": {"auth": {"api_key": "${env:KEY}"}}}});
        assert_eq!(
            redact_change(&reference, &[]),
            reference,
            "a reference is not a secret"
        );
    }

    #[test]
    fn known_paths_redact_what_the_schema_alone_does_not_reach() {
        let change = json!({"method": "PATCH", "path": "/settings/smtp", "body": {"spec": {"odd": "x"}}});
        let known = [DocPath::root().key("smtp").key("odd")];
        assert_eq!(
            redact_change(&change, &known).pointer("/body/spec/odd"),
            Some(&json!(secrets::REDACTED))
        );
    }

    #[test]
    fn the_literals_are_sealed_opened_and_forgotten() {
        let s = sealer();
        let raw = vec![json!({"method": "PATCH", "path": "/settings/smtp",
            "body": {"spec": {"password": "hunter2"}}})];
        let shown = redact_changes(&raw, &[]);
        let mut body = json!({"id": "cr_1", "changes": shown});
        keep(&s, &mut body, "cr_1", &raw, &shown).unwrap_or_else(|_| unreachable!());
        assert!(!body.to_string().contains("hunter2"));
        assert_eq!(literal(&s, &body).ok(), Some(raw));
        assert!(forget(&mut body));
        assert_eq!(literal(&s, &body).ok(), Some(shown.clone()));
        let mut plain = json!({"id": "cr_2", "changes": shown});
        keep(&s, &mut plain, "cr_2", &shown, &shown).unwrap_or_else(|_| unreachable!());
        assert!(plain.get(SEALED).is_none());
    }
}

//! Rules of `security.secrets.stores` the schema cannot express: a store's own settings resolve
//! without any store, and each authentication method has the fields it needs and no other's.

use serde_json::Value;

use crate::secrets::store_references;
use crate::{ConfigError, DetailCode, DocPath};

/// The fields each Vault authentication method needs.
const VAULT_METHODS: [(&str, &[&str]); 3] = [
    ("token", &["token"]),
    ("approle", &["role_id", "secret_id"]),
    ("kubernetes", &["role"]),
];

fn set(v: &Value, k: &str) -> bool {
    v.get(k).is_some_and(|x| !x.is_null())
}

/// Every string under `v` that holds a store reference, with its path.
fn nested_references(v: &Value, at: &DocPath, out: &mut Vec<DocPath>) {
    match v {
        Value::String(_) if !store_references(v).is_empty() => out.push(at.clone()),
        Value::Array(items) => {
            for (i, x) in items.iter().enumerate() {
                nested_references(x, &at.index(i), out);
            }
        }
        Value::Object(map) => {
            for (k, x) in map {
                nested_references(x, &at.key(k), out);
            }
        }
        _ => {}
    }
}

fn vault(v: &Value, at: &DocPath, errors: &mut Vec<ConfigError>) {
    let Some(auth) = v.get("auth") else { return };
    let at = at.key("auth");
    let method = auth.get("method").and_then(Value::as_str).unwrap_or("token");
    for (name, fields) in VAULT_METHODS {
        for field in fields.iter() {
            if name == method && !set(auth, field) {
                errors.push(ConfigError::new(
                    &at.key(field),
                    DetailCode::Required,
                    format!("required with method {method}"),
                ));
            } else if name != method && set(auth, field) {
                errors.push(ConfigError::new(
                    &at.key(field),
                    DetailCode::Conflict,
                    format!("belongs to method {name}, not {method}"),
                ));
            }
        }
    }
}

fn aws(v: &Value, at: &DocPath, errors: &mut Vec<ConfigError>) {
    if set(v, "access_key_id") != set(v, "secret_access_key") {
        errors.push(ConfigError::new(
            at,
            DetailCode::Conflict,
            "static credentials need both access_key_id and secret_access_key",
        ));
    }
    if set(v, "session_token") && !set(v, "access_key_id") {
        errors.push(ConfigError::new(
            &at.key("session_token"),
            DetailCode::Conflict,
            "a session token goes with access_key_id and secret_access_key",
        ));
    }
}

fn azure(v: &Value, at: &DocPath, errors: &mut Vec<ConfigError>) {
    if v.get("managed_identity").and_then(Value::as_bool) == Some(true) {
        for field in ["client_secret", "token_file"].iter().filter(|f| set(v, f)) {
            errors.push(ConfigError::new(
                &at.key(field),
                DetailCode::Conflict,
                "not used with managed_identity",
            ));
        }
        return;
    }
    for field in ["tenant_id", "client_id"].iter().filter(|f| !set(v, f)) {
        errors.push(ConfigError::new(
            &at.key(field),
            DetailCode::Required,
            "required unless managed_identity is true",
        ));
    }
}

pub(crate) fn check(doc: &Value, errors: &mut Vec<ConfigError>) {
    let Some(stores) = doc.pointer("/security/secrets/stores") else {
        return;
    };
    let at = DocPath::root().key("security").key("secrets").key("stores");
    let mut nested = Vec::new();
    nested_references(stores, &at, &mut nested);
    for path in nested {
        errors.push(ConfigError::new(
            &path,
            DetailCode::InvalidFormat,
            "a secret store's settings cannot reference a secret store",
        ));
    }
    if let Some(v) = stores.get("vault") {
        vault(v, &at.key("vault"), errors);
    }
    if let Some(v) = stores.get("aws_sm") {
        aws(v, &at.key("aws_sm"), errors);
    }
    if let Some(v) = stores.get("azure_kv") {
        azure(v, &at.key("azure_kv"), errors);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn paths(stores: Value) -> Vec<String> {
        let mut errors = Vec::new();
        check(&json!({"security": {"secrets": {"stores": stores}}}), &mut errors);
        errors.into_iter().map(|e| e.path).collect()
    }

    #[test]
    fn settings_resolve_without_stores() {
        assert_eq!(
            paths(json!({"vault": {"address": "https://v", "auth": {"token": "${aws-sm:t}"}}})),
            ["security.secrets.stores.vault.auth.token"]
        );
        assert!(paths(json!({"vault": {"address": "https://v", "auth": {"token": "${env:T}"}}})).is_empty());
    }

    #[test]
    fn each_vault_method_has_its_fields() {
        assert_eq!(
            paths(
                json!({"vault": {"address": "https://v", "auth": {"method": "approle", "role_id": "r", "token": "t"}}})
            ),
            [
                "security.secrets.stores.vault.auth.token",
                "security.secrets.stores.vault.auth.secret_id"
            ]
        );
        assert_eq!(
            paths(json!({"vault": {"address": "https://v", "auth": {"method": "kubernetes"}}})),
            ["security.secrets.stores.vault.auth.role"]
        );
        assert!(
            paths(json!({"vault": {"address": "https://v", "auth": {"method": "kubernetes", "role": "gw", "jwt_file": "/t"}}}))
                .is_empty()
        );
    }

    #[test]
    fn cloud_credentials_are_whole() {
        assert_eq!(
            paths(json!({"aws_sm": {"access_key_id": "a"}})),
            ["security.secrets.stores.aws_sm"]
        );
        assert!(paths(json!({"aws_sm": {}})).is_empty());
        assert_eq!(
            paths(json!({"azure_kv": {"tenant_id": "t"}})),
            ["security.secrets.stores.azure_kv.client_id"]
        );
        assert_eq!(
            paths(json!({"azure_kv": {"managed_identity": true, "client_secret": "s"}})),
            ["security.secrets.stores.azure_kv.client_secret"]
        );
    }
}

//! `api.passthrough` mounts: each names an existing provider, and its prefix overlaps neither the
//! endpoint catalog nor another mount's prefix.

use serde_json::Value;

use crate::{ConfigError, DetailCode, DocPath};

/// First path segments the endpoint catalog, the probes and the provider-style mounts serve.
const RESERVED: &[&str] = &[
    "v1",
    "v1beta",
    "v2",
    "openai",
    "anthropic",
    "gemini",
    "bedrock",
    "model",
    "mcp",
    "a2a",
    ".well-known",
    "-",
    "chat",
    "embeddings",
    "models",
    "responses",
    "completions",
    "engines",
];

fn segments(prefix: &str) -> Vec<&str> {
    prefix.split('/').filter(|s| !s.is_empty()).collect()
}

/// Whether one prefix is the other, or a path below it.
fn overlaps(a: &[&str], b: &[&str]) -> bool {
    a.iter().zip(b).all(|(x, y)| x == y)
}

pub(crate) fn check(doc: &Value, errors: &mut Vec<ConfigError>) {
    let Some(mounts) = doc.pointer("/api/passthrough").and_then(Value::as_object) else {
        return;
    };
    let base = DocPath::root().key("api").key("passthrough");
    let mut seen: Vec<(&String, Vec<&str>)> = Vec::new();
    for (name, m) in mounts {
        let at = base.key(name);
        if let Some(p) = m.get("provider").and_then(Value::as_str)
            && doc.pointer(&format!("/providers/{p}")).is_none()
        {
            errors.push(ConfigError::new(
                &at.key("provider"),
                DetailCode::UnresolvedReference,
                format!("provider {p:?} does not exist"),
            ));
        }
        let Some(prefix) = m.get("path_prefix").and_then(Value::as_str) else {
            continue;
        };
        let segs = segments(prefix);
        let bad_segment = segs
            .iter()
            .any(|s| matches!(*s, "." | "..") || s.contains(['%', '\\', '?', '#']));
        let reserved = segs.first().is_none_or(|first| RESERVED.contains(first));
        if bad_segment || reserved {
            errors.push(ConfigError::new(
                &at.key("path_prefix"),
                DetailCode::Conflict,
                "the prefix overlaps the endpoint catalog or is not a plain path",
            ));
            continue;
        }
        if let Some((other, _)) = seen.iter().find(|(_, s)| overlaps(s, &segs)) {
            errors.push(ConfigError::new(
                &at.key("path_prefix"),
                DetailCode::Conflict,
                format!("the prefix overlaps the prefix of mount {other:?}"),
            ));
            continue;
        }
        seen.push((name, segs));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn paths(doc: &Value) -> Vec<String> {
        let mut errors = Vec::new();
        check(doc, &mut errors);
        errors.into_iter().map(|e| e.path).collect()
    }

    #[test]
    fn prefixes_are_checked() {
        let doc = json!({
            "providers": {"p": {}},
            "api": {"passthrough": {
                "a": {"path_prefix": "/pt/openai", "provider": "p"},
                "b": {"path_prefix": "/pt/openai/sub", "provider": "p"},
                "c": {"path_prefix": "/v1/raw", "provider": "p"},
                "d": {"path_prefix": "/", "provider": "p"},
                "e": {"path_prefix": "/other", "provider": "missing"},
                "f": {"path_prefix": "/x/../y", "provider": "p"}
            }}
        });
        let got = paths(&doc);
        for want in [
            "api.passthrough.b.path_prefix",
            "api.passthrough.c.path_prefix",
            "api.passthrough.d.path_prefix",
            "api.passthrough.e.provider",
            "api.passthrough.f.path_prefix",
        ] {
            assert!(got.iter().any(|p| p == want), "{want} not in {got:?}");
        }
        assert!(!got.iter().any(|p| p.starts_with("api.passthrough.a.")));
    }
}

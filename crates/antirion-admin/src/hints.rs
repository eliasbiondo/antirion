//! Last-four hints of provider credentials and event webhook signing secrets: a literal secret of
//! at least 16 characters shows as `…abcd` in `metadata.secret_hints`, keyed by its path in the
//! resource or settings; a secret configured by reference shows no hint. Hints are only in resource
//! and settings reads, never in exports or diffs.

use antirion_config::DocPath;
use antirion_config::secrets::has_reference;
use antirion_runtime::Snapshot;
use serde_json::{Map, Value};

/// The shortest secret that gets a hint.
const MIN_LEN: usize = 16;

/// The hint of one secret value, when it gets one.
fn hint(secret: &str) -> Option<String> {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() < MIN_LEN || has_reference(secret) || secret.starts_with("enc:") {
        return None;
    }
    let last: String = chars.get(chars.len() - 4..)?.iter().collect();
    Some(format!("…{last}"))
}

/// The hints of the secrets under `prefix` that `keep` selects, keyed by their path after it.
fn under(snap: &Snapshot, prefix: &str, keep: impl Fn(&str) -> bool) -> Map<String, Value> {
    let mut out = Map::new();
    for path in snap.secret_paths.iter() {
        let rendered = path.to_string();
        let Some(field) = rendered.strip_prefix(prefix) else {
            continue;
        };
        if !keep(field) {
            continue;
        }
        if let Some(Value::String(s)) = DocPath::get(path, &snap.config.doc)
            && let Some(h) = hint(s)
        {
            out.insert(field.to_owned(), Value::String(h));
        }
    }
    out
}

/// Adds `metadata.secret_hints` to a provider's representation.
pub(crate) fn decorate(snap: &Snapshot, name: &str, body: &mut Value) {
    let hints = under(snap, &format!("providers.{name}."), |_| true);
    insert(body, hints);
}

/// Adds `metadata.secret_hints` to the observability settings: each event webhook's signing secret,
/// keyed `alerts.webhooks.<name>.signing_secret`.
pub(crate) fn decorate_observability(snap: &Snapshot, body: &mut Value) {
    let hints = under(snap, "observability.", |field| {
        field.starts_with("alerts.webhooks.") && field.ends_with(".signing_secret")
    });
    insert(body, hints);
}

fn insert(body: &mut Value, hints: Map<String, Value>) {
    if let Some(m) = body.get_mut("metadata").and_then(Value::as_object_mut) {
        m.insert("secret_hints".into(), Value::Object(hints));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_literals_get_their_last_four() {
        assert_eq!(hint("sk-test-0123456789abcdWXYZ").as_deref(), Some("…WXYZ"));
        assert_eq!(hint("short-secret"), None);
        assert_eq!(hint("${env:PROVIDER_KEY_LONG_NAME}"), None);
    }
}

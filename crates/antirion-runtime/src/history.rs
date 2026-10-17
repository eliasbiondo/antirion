//! The generation history: every committed generation's document is kept as stored (literal
//! secrets sealed) for the newest `admin.config.history_limit` generations, to be read back with
//! secrets redacted and rolled back to.

use antirion_core::audit::ActorKind;
use antirion_store::StoreError;
use serde_json::Value;

use crate::Runtime;

const DEFAULT_LIMIT: u64 = 100;
const SEALED: &str = crate::atrest::PREFIX;

/// The `type` of an actor as audit entries and generation summaries name it.
#[must_use]
pub fn actor_type(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Operator => "operator",
        ActorKind::Token => "token",
        ActorKind::System => "system",
    }
}

/// Replaces every sealed secret of a stored document with `[REDACTED]`.
pub fn redact_sealed(v: &mut Value) {
    match v {
        Value::String(s) if s.starts_with(SEALED) => *s = "[REDACTED]".to_owned(),
        Value::Array(items) => items.iter_mut().for_each(redact_sealed),
        Value::Object(m) => m.values_mut().for_each(redact_sealed),
        _ => {}
    }
}

impl Runtime {
    /// `admin.config.history_limit`.
    #[must_use]
    pub fn history_limit(&self) -> u64 {
        self.statics
            .doc
            .pointer("/admin/config/history_limit")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_LIMIT)
    }

    pub(crate) async fn keep_generation(&self, generation: u64, document: String, actor: ActorKind) {
        let kept = self
            .store
            .put_generation_document(
                generation,
                document,
                actor_type(actor).to_owned(),
                self.history_limit(),
            )
            .await;
        if let Err(e) = kept {
            tracing::warn!(error = %e, generation, "a generation's document could not be kept");
        }
    }

    /// A kept generation's document as stored (secrets sealed), unless it is older than the
    /// history keeps.
    pub async fn kept_generation(&self, generation: u64) -> Result<Option<Value>, StoreError> {
        let current = self.gens.load().generation;
        if generation == 0 || generation > current || current - generation >= self.history_limit() {
            return Ok(None);
        }
        let Some((doc, _)) = self.store.generation_document(generation).await? else {
            return Ok(None);
        };
        Ok(serde_json::from_str(&doc).ok())
    }

    /// A kept generation's document with its secrets opened, for a rollback.
    pub async fn opened_generation(&self, generation: u64) -> Result<Option<Value>, StoreError> {
        let Some(mut doc) = self.kept_generation(generation).await? else {
            return Ok(None);
        };
        if self.sealer.open_document(&mut doc).is_err() {
            return Ok(None);
        }
        Ok(Some(doc))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn sealed_values_are_redacted_wherever_they_are() {
        let mut v = json!({"a": "enc:v1:AAAA", "b": ["x", "enc:v1:BB"], "c": {"d": "plain"}});
        redact_sealed(&mut v);
        assert_eq!(
            v,
            json!({"a": "[REDACTED]", "b": ["x", "[REDACTED]"], "c": {"d": "plain"}})
        );
    }
}

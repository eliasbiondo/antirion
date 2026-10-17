//! The audit-log record, shared by the admin plane that writes it and the store that keeps it.

use serde::{Deserialize, Serialize};

/// Who made an admin call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    #[serde(rename = "type")]
    pub kind: ActorKind,
    pub id: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Operator,
    Token,
    System,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Denied,
    Failed,
}

impl Outcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::Denied => "denied",
            Outcome::Failed => "failed",
        }
    }
}

/// Whether an entry records a change (or an attempt at one) or a sensitive read.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    #[default]
    Write,
    Read,
}

impl AuditKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AuditKind::Write => "write",
            AuditKind::Read => "read",
        }
    }
}

/// One audit entry. `time` is RFC 3339; `seq` orders entries and is not part of the API shape.
///
/// `changes` is a change set (a list of `{op, path, before, after}`) or null. Actions that commit
/// no generation record what they did as facts; an entry read back states them as one change of
/// its resource (see [`change_list`]), so entries stored with the facts as an object read the same.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "Stored")]
pub struct AuditEntry {
    pub id: String,
    pub time: String,
    pub actor: Actor,
    pub action: String,
    pub resource: String,
    pub outcome: Outcome,
    pub status_code: u16,
    pub generation_before: Option<u64>,
    pub generation_after: Option<u64>,
    pub request_id: String,
    pub source_ip: Option<String>,
    pub changes: serde_json::Value,
    #[serde(default)]
    pub kind: AuditKind,
    /// The operator's stated reason for the change, when one was sent.
    #[serde(default)]
    pub reason: Option<String>,
}

/// An audit entry as stored, before its `changes` are read as a change set.
#[derive(Deserialize)]
struct Stored {
    id: String,
    time: String,
    actor: Actor,
    action: String,
    resource: String,
    outcome: Outcome,
    status_code: u16,
    generation_before: Option<u64>,
    generation_after: Option<u64>,
    request_id: String,
    source_ip: Option<String>,
    changes: serde_json::Value,
    #[serde(default)]
    kind: AuditKind,
    #[serde(default)]
    reason: Option<String>,
}

impl From<Stored> for AuditEntry {
    fn from(s: Stored) -> Self {
        AuditEntry {
            changes: change_list(&s.resource, s.changes),
            id: s.id,
            time: s.time,
            actor: s.actor,
            action: s.action,
            resource: s.resource,
            outcome: s.outcome,
            status_code: s.status_code,
            generation_before: s.generation_before,
            generation_after: s.generation_after,
            request_id: s.request_id,
            source_ip: s.source_ip,
            kind: s.kind,
            reason: s.reason,
        }
    }
}

/// An entry's `changes` as the Admin API has them: a change set, or null. The facts an action
/// records (an object, or a lone value) become one `update` of the entry's resource, at the
/// resource's path with `/` read as `.`, with the facts as `after` (a lone value as `{"value": v}`)
/// and `before` null.
#[must_use]
pub fn change_list(resource: &str, changes: serde_json::Value) -> serde_json::Value {
    use serde_json::{Value, json};
    let after = match changes {
        Value::Null | Value::Array(_) => return changes,
        Value::Object(_) => changes,
        other => json!({ "value": other }),
    };
    json!([{ "op": "update", "path": resource.replace('/', "."), "before": null, "after": after }])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn stored(changes: serde_json::Value) -> AuditEntry {
        serde_json::from_value(json!({
            "id": "a", "time": "2026-10-15T00:00:00Z", "actor": {"type": "operator", "id": "u"},
            "action": "routes.affinity.delete", "resource": "routes/chat/affinity", "outcome": "success",
            "status_code": 200, "generation_before": 3, "generation_after": 3, "request_id": "r",
            "source_ip": null, "changes": changes
        }))
        .unwrap()
    }

    #[test]
    fn facts_read_back_as_one_change_of_the_resource() {
        let e = stored(json!({"deleted": 2}));
        assert_eq!(
            e.changes,
            json!([{"op": "update", "path": "routes.chat.affinity", "before": null, "after": {"deleted": 2}}])
        );
        let e = stored(json!(true));
        assert_eq!(e.changes[0]["after"], json!({"value": true}));
    }

    #[test]
    fn change_sets_and_null_are_kept() {
        let set = json!([{"op": "create", "path": "routes.x", "before": null, "after": {"tree": "a"}}]);
        assert_eq!(stored(set.clone()).changes, set);
        assert_eq!(stored(serde_json::Value::Null).changes, serde_json::Value::Null);
        let again: AuditEntry =
            serde_json::from_value(serde_json::to_value(stored(json!({"deleted": 1}))).unwrap_or_default())
                .unwrap();
        assert_eq!(
            again.changes.as_array().map(Vec::len),
            Some(1),
            "reading twice changes nothing"
        );
    }
}

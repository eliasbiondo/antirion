//! MCP tool-definition approvals kept in the store, so they survive a restart.

use std::collections::BTreeMap;

use antirion_core::UtcMicros;
use serde_json::{Map, Value, json};

use crate::Runtime;

const APPROVALS_KEY: &str = "mcp_tool_approvals";

impl Runtime {
    /// Writes every approval this node holds to the store, keeping any newer stored entry.
    pub async fn persist_mcp_approvals(&self) {
        let bytes = match self.store.get_or_init(APPROVALS_KEY, b"{}".to_vec()).await {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(error = %e, "the stored MCP tool approvals could not be read");
                return;
            }
        };
        let mut stored: Map<String, Value> = serde_json::from_slice(&bytes).unwrap_or_default();
        merge_approvals(&mut stored, self.state.mcp.approvals());
        let bytes = serde_json::to_vec(&Value::Object(stored)).unwrap_or_default();
        if let Err(e) = self.store.put_value(APPROVALS_KEY, bytes).await {
            tracing::warn!(error = %e, "the MCP tool approvals could not be stored");
        }
    }

    /// Restores every tool approval a previous run stored; entries that cannot be read are skipped.
    pub async fn restore_mcp_approvals(&self) {
        let Ok(bytes) = self.store.get_or_init(APPROVALS_KEY, b"{}".to_vec()).await else {
            return;
        };
        let Ok(stored) = serde_json::from_slice::<Value>(&bytes) else {
            return;
        };
        let Some(entries) = stored.as_object() else {
            return;
        };
        for (server, entry) in entries {
            if let Some((approved_at, tools)) = read_stored_approval(entry) {
                self.state.mcp.restore_approval(server, approved_at, tools);
            }
        }
    }

    /// Forgets every stored tool approval (the test reset's `cache` scope).
    pub async fn forget_mcp_approvals(&self) {
        if let Err(e) = self.store.put_value(APPROVALS_KEY, b"{}".to_vec()).await {
            tracing::warn!(error = %e, "the MCP tool approvals could not be cleared");
        }
    }
}

/// Merges this node's approvals into `stored`: an entry is written when the stored one is missing,
/// older or unreadable, and a newer or equally new stored entry is kept.
fn merge_approvals(
    stored: &mut Map<String, Value>,
    approvals: Vec<(String, UtcMicros, BTreeMap<String, String>)>,
) {
    for (server, approved_at, tools) in approvals {
        let stored_at = stored
            .get(&server)
            .and_then(read_stored_approval)
            .map(|(at, _)| at);
        if stored_at.is_some_and(|at| at >= approved_at) {
            continue;
        }
        stored.insert(
            server,
            json!({"approved_at": approved_at.to_rfc3339(), "tools": tools}),
        );
    }
}

/// The approval a stored entry holds, when both its time and its tools can be read.
fn read_stored_approval(entry: &Value) -> Option<(UtcMicros, BTreeMap<String, String>)> {
    let approved_at = entry
        .get("approved_at")
        .and_then(Value::as_str)
        .and_then(|s| UtcMicros::parse_rfc3339(s).ok())?;
    let tools = entry
        .get("tools")
        .and_then(|t| serde_json::from_value::<BTreeMap<String, String>>(t.clone()).ok())?;
    Some((approved_at, tools))
}

#[cfg(test)]
mod tests {
    use super::*;

    type NodeEntry<'a> = (&'a str, i64, &'a [(&'a str, &'a str)]);

    fn node_approvals(entries: &[NodeEntry<'_>]) -> Vec<(String, UtcMicros, BTreeMap<String, String>)> {
        entries
            .iter()
            .map(|(server, at, tools)| {
                (
                    (*server).to_owned(),
                    UtcMicros(*at),
                    tools
                        .iter()
                        .map(|(tool, fingerprint)| ((*tool).to_owned(), (*fingerprint).to_owned()))
                        .collect(),
                )
            })
            .collect()
    }

    fn merged(stored: Value, node: &[NodeEntry<'_>]) -> Value {
        let mut stored = stored.as_object().cloned().unwrap_or_default();
        merge_approvals(&mut stored, node_approvals(node));
        Value::Object(stored)
    }

    #[test]
    fn a_missing_entry_is_added() {
        let out = merged(json!({}), &[("crm", 2_000_000, &[("search", "a")])]);
        assert_eq!(out["crm"]["tools"]["search"], json!("a"));
        assert_eq!(
            out["crm"]["approved_at"],
            json!(UtcMicros(2_000_000).to_rfc3339())
        );
    }

    #[test]
    fn an_older_stored_entry_is_replaced() {
        let out = merged(
            json!({"crm": {"approved_at": UtcMicros(1_000_000).to_rfc3339(),
                             "tools": {"search": "old"}}}),
            &[("crm", 2_000_000, &[("search", "new")])],
        );
        assert_eq!(out["crm"]["tools"]["search"], json!("new"));
        assert_eq!(
            out["crm"]["approved_at"],
            json!(UtcMicros(2_000_000).to_rfc3339())
        );
    }

    #[test]
    fn a_newer_stored_entry_is_kept() {
        let stored = json!({"crm": {"approved_at": UtcMicros(3_000_000).to_rfc3339(),
                                   "tools": {"search": "stored"}}});
        let out = merged(stored.clone(), &[("crm", 2_000_000, &[("search", "node")])]);
        assert_eq!(out, stored);
    }

    #[test]
    fn another_servers_entry_is_kept() {
        let out = merged(
            json!({"other": {"approved_at": UtcMicros(1_000_000).to_rfc3339(),
                              "tools": {"x": "f"}}}),
            &[("crm", 2_000_000, &[("search", "a")])],
        );
        assert_eq!(out["other"]["tools"]["x"], json!("f"));
        assert_eq!(out["crm"]["tools"]["search"], json!("a"));
    }

    #[test]
    fn an_unreadable_stored_entry_is_replaced() {
        let out = merged(
            json!({"crm": {"approved_at": "not a time", "tools": {}}}),
            &[("crm", 2_000_000, &[("search", "a")])],
        );
        assert_eq!(out["crm"]["tools"]["search"], json!("a"));
        assert_eq!(
            out["crm"]["approved_at"],
            json!(UtcMicros(2_000_000).to_rfc3339())
        );
    }
}

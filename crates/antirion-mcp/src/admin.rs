//! What the Admin API reads and changes: a server's discovery status, approval of its current tool
//! definitions, and per-gateway session counters.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::compile::McpSnapshot;
use crate::connect::{gateway_client_info, open_with};
use crate::fingerprint;
use crate::http::Env;
use crate::state::{Approval, McpState};
use crate::upstream::LATEST_LEGACY;

/// Session counters of one gateway (`/runtime/mcp/sessions`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionStats {
    pub gateway: String,
    pub session_mode: &'static str,
    pub issued: u64,
    pub rejected: u64,
    pub revoked: usize,
    /// Ids of the keys that decrypt; the first encrypts.
    pub key_ids: Vec<String>,
}

/// One entry per configured gateway, in name order.
#[must_use]
pub fn session_stats(snap: &McpSnapshot, state: &McpState) -> Vec<SessionStats> {
    let counts = state.counts.lock().clone();
    let keys: Vec<String> = state
        .keys(&snap.session_keys)
        .iter()
        .map(|k| k.id.to_string())
        .collect();
    snap.gateways
        .values()
        .map(|g| {
            let c = counts.get(&g.name).copied().unwrap_or_default();
            SessionStats {
                gateway: g.name.clone(),
                session_mode: if g.stateless { "stateless" } else { "stateful" },
                issued: c.issued,
                rejected: c.rejected,
                revoked: state.revoked_count(),
                key_ids: keys.clone(),
            }
        })
        .collect()
}

/// `GET /mcp/servers/<name>/status`; `None` for an unknown server.
#[must_use]
pub fn server_status(snap: &McpSnapshot, state: &McpState, name: &str) -> Option<Value> {
    let server = snap.servers.get(name)?;
    let processes = server.stdio.as_ref().map(|spec| {
        let (started, running) = state.stdio_started(server.generation).unwrap_or((0, 0));
        json!({"size": spec.processes, "running": running, "started": started})
    });
    let facts = state.facts.lock().get(name).cloned().unwrap_or_default();
    let era = match facts.modern {
        Some(true) => "modern",
        Some(false) => "legacy",
        None => "unknown",
    };
    let seen = state.fingerprints.lock().get(name).cloned().unwrap_or_default();
    let approval = state.approvals.lock().get(name).cloned();
    let tools: Vec<Value> = seen
        .iter()
        .map(|(tool, fp)| {
            let approved = approval
                .as_ref()
                .map(|a| a.tools.get(tool).is_some_and(|ok| ok == fp));
            let hidden = server.pin_tool_definitions && approved != Some(true);
            json!({"name": tool, "fingerprint": fp, "approved": approved, "hidden": hidden})
        })
        .collect();
    Some(json!({
        "era_capability": era,
        "last_discovery_at": facts.last_discovery_at.map(|t| t.to_rfc3339_seconds()),
        "last_error": facts.last_error,
        "cached_tools": seen.len(),
        "processes": processes,
        "tools": tools,
        "approved_at": approval.map(|a| a.approved_at.to_rfc3339_seconds()),
        "last_contact_at": facts.last_contact_at.map(|t| t.to_rfc3339_seconds()),
        "last_contact_error": facts.last_contact_error,
    }))
}

/// The fingerprints an approval recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolApproval {
    pub server: String,
    pub approved_at: String,
    pub tools: Vec<(String, String)>,
}

impl ToolApproval {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "server": self.server,
            "approved_at": self.approved_at,
            "tools": self.tools.iter().map(|(n, f)| json!({"name": n, "fingerprint": f})).collect::<Vec<_>>(),
        })
    }
}

/// Why tools could not be approved.
#[derive(Debug, PartialEq, Eq)]
pub enum ApproveError {
    UnknownServer,
    Unreachable,
}

/// Records the current fingerprint of every tool `name` lists, reading the list now.
pub async fn approve_tools(env: &Env, name: &str) -> Result<ToolApproval, ApproveError> {
    let server = env
        .snap
        .servers
        .get(name)
        .cloned()
        .ok_or(ApproveError::UnknownServer)?;
    let (link, _) = open_with(
        env,
        &server,
        http::HeaderMap::new(),
        LATEST_LEGACY,
        &json!({}),
        &gateway_client_info(),
        false,
    )
    .await
    .map_err(|_| ApproveError::Unreachable)?;
    let mut tools = Vec::new();
    let mut cursor: Option<Value> = None;
    let mut ok = true;
    for _ in 0..1000 {
        let params = cursor.take().map(|c| json!({"cursor": c}));
        let Ok(msg) = link.ask(env, "tools/list", params).await else {
            ok = false;
            break;
        };
        let Some(result) = msg.get("result") else {
            ok = false;
            break;
        };
        tools.extend(
            result
                .get("tools")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        match result.get("nextCursor").filter(|c| !c.is_null()) {
            Some(c) => cursor = Some(c.clone()),
            None => break,
        }
    }
    link.close(env).await;
    if !ok {
        return Err(ApproveError::Unreachable);
    }
    fingerprint::observe(&env.state, &server, &tools);
    let approved: BTreeMap<String, String> = tools
        .iter()
        .filter_map(|t| Some((t.get("name")?.as_str()?.to_owned(), fingerprint::of(t))))
        .collect();
    env.state.approvals.lock().insert(
        server.name.clone(),
        Approval {
            tools: approved.clone(),
            approved_at: env.now,
        },
    );
    Ok(ToolApproval {
        server: server.name.clone(),
        approved_at: env.now.to_rfc3339_seconds(),
        tools: approved.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use antirion_core::UtcMicros;
    use serde_json::json;

    use super::*;
    use crate::compile::compile;

    fn snapshot(pin: bool) -> McpSnapshot {
        compile(&json!({"mcp": {"servers": {"a": {
            "transport": "streamable_http", "url": "http://127.0.0.1:1/mcp", "pin_tool_definitions": pin}}}}))
        .unwrap()
    }

    #[test]
    fn status_lists_seen_tools_with_their_approval() {
        let snap = snapshot(true);
        let state = McpState::default();
        let empty = server_status(&snap, &state, "a").unwrap();
        assert_eq!(empty["tools"], json!([]));
        assert_eq!(empty["approved_at"], Value::Null);

        let add = json!({"name": "add", "inputSchema": {"type": "object"}});
        let sub = json!({"name": "sub", "inputSchema": {"type": "object"}});
        fingerprint::observe(&state, &snap.servers["a"], &[add.clone(), sub.clone()]);
        let unapproved = server_status(&snap, &state, "a").unwrap();
        assert_eq!(unapproved["tools"][0]["approved"], Value::Null);
        assert_eq!(unapproved["tools"][0]["hidden"], json!(true));
        assert_eq!(unapproved["cached_tools"], json!(2));

        state.approvals.lock().insert(
            "a".to_owned(),
            Approval {
                tools: BTreeMap::from([("add".to_owned(), fingerprint::of(&add))]),
                approved_at: UtcMicros::EPOCH,
            },
        );
        let status = server_status(&snap, &state, "a").unwrap();
        assert_eq!(status["tools"][0]["name"], json!("add"));
        assert_eq!(status["tools"][0]["approved"], json!(true));
        assert_eq!(status["tools"][0]["hidden"], json!(false));
        assert_eq!(status["tools"][1]["approved"], json!(false));
        assert_eq!(status["tools"][1]["hidden"], json!(true));
        assert!(status["approved_at"].is_string());
    }

    #[test]
    fn an_unpinned_server_hides_nothing() {
        let snap = snapshot(false);
        let state = McpState::default();
        fingerprint::observe(&state, &snap.servers["a"], &[json!({"name": "add"})]);
        let status = server_status(&snap, &state, "a").unwrap();
        assert_eq!(status["tools"][0]["hidden"], json!(false));
        assert_eq!(status["last_contact_at"], Value::Null);
        assert!(server_status(&snap, &state, "missing").is_none());
    }

    #[test]
    fn the_last_contact_and_its_error_are_reported() {
        let snap = snapshot(false);
        let state = McpState::default();
        state.record_contact("a", UtcMicros::EPOCH, Some("could not connect".to_owned()));
        let failed = server_status(&snap, &state, "a").unwrap();
        assert_eq!(failed["last_contact_error"], json!("could not connect"));
        assert!(failed["last_contact_at"].is_string());
        state.record_contact("a", UtcMicros::EPOCH, None);
        assert_eq!(
            server_status(&snap, &state, "a").unwrap()["last_contact_error"],
            Value::Null
        );
    }
}

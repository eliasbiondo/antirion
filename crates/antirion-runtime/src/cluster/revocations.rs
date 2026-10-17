//! MCP session revocations are shared through the counter store: each node writes the ones it made
//! (kept until the token would expire) and adds the others' to its own set, every second. While the
//! counter store is down, each node checks only the revocations it knows (upstreams still refuse
//! deleted sessions).

use std::sync::Arc;
use std::time::Duration;

use super::pause;
use crate::Runtime;

const EVERY: Duration = Duration::from_secs(1);

fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    if s.len() != 64 {
        return None;
    }
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// How long a request waits for the shared revocation set before it skips the check.
const CHECK_TIMEOUT: Duration = Duration::from_millis(250);

impl Runtime {
    /// Before a request carrying an MCP session token: learns whether another node revoked it.
    /// False when the shared set could not be read, so the check was skipped.
    pub async fn check_shared_revocation(&self, token: &str) -> bool {
        let engine = self.accounts.engine();
        let Some(valkey) = engine.store().valkey() else {
            return true;
        };
        let (digest, known) = self.state.mcp.revocation_of(token);
        if known.is_some() {
            return true;
        }
        if engine.is_store_down() {
            return false;
        }
        match tokio::time::timeout(CHECK_TIMEOUT, valkey.revocation(&hex(&digest))).await {
            Ok(Ok(Some(exp))) => {
                self.state.mcp.add_revocations([(digest, exp)]);
                true
            }
            Ok(Ok(None)) => true,
            _ => false,
        }
    }

    /// After a session was deleted on this node: shares its revocation at once, so the next
    /// request on any node is refused.
    pub async fn share_revocation(&self, token: &str) {
        let engine = self.accounts.engine();
        let Some(valkey) = engine.store().valkey() else {
            return;
        };
        if engine.is_store_down() {
            return;
        }
        if let (digest, Some(exp)) = self.state.mcp.revocation_of(token) {
            let item = [(hex(&digest), exp)];
            let shared = valkey.share_revocations(&item);
            if !matches!(tokio::time::timeout(CHECK_TIMEOUT, shared).await, Ok(Ok(()))) {
                tracing::debug!("a session revocation is shared with the next round");
            }
        }
    }
}

pub(crate) async fn run(rt: Arc<Runtime>) {
    while pause(&rt, EVERY).await {
        let engine = rt.accounts.engine();
        let Some(valkey) = engine.store().valkey() else {
            return;
        };
        if engine.is_store_down() {
            continue;
        }
        let mine: Vec<(String, i64)> = rt
            .state
            .mcp
            .revocations()
            .iter()
            .map(|(d, e)| (hex(d), *e))
            .collect();
        if valkey.share_revocations(&mine).await.is_err() {
            continue;
        }
        if let Ok(all) = valkey.revocations().await {
            rt.state
                .mcp
                .add_revocations(all.iter().filter_map(|(d, e)| Some((unhex(d)?, *e))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_round_trip_through_hex() {
        let d = [0xabu8; 32];
        assert_eq!(unhex(&hex(&d)), Some(d));
        assert_eq!(unhex("ab"), None);
    }
}

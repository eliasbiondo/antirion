//! Attempts in flight shared through the counter store, for `least_busy` nodes of cluster scope:
//! every `EVERY`, the node writes its own counts and reads the other nodes'. A request never waits
//! on the store; counts older than the routing state's freshness bound are not used.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::pause;
use crate::Runtime;

const EVERY: Duration = Duration::from_millis(250);
/// Another node's counts older than this are left out.
const STALE_MS: u64 = 2_000;

pub(crate) async fn run(rt: Arc<Runtime>) {
    while pause(&rt, EVERY).await {
        let engine = rt.accounts.engine();
        let Some(valkey) = engine.store().valkey() else {
            return;
        };
        let mine = rt.state.routing.local_inflight();
        let now = u64::try_from(rt.now().0 / 1000).unwrap_or(0);
        let own = json!({"at_ms": now, "counts": mine}).to_string();
        if valkey.inflight_put(&rt.node.id, &own).await.is_err() {
            continue;
        }
        let Ok(all) = valkey.inflight_all().await else {
            continue;
        };
        let mut others: HashMap<String, u32> = HashMap::new();
        for (_, text) in all.iter().filter(|(n, _)| *n != rt.node.id) {
            let Ok(v) = serde_json::from_str::<Value>(text) else {
                continue;
            };
            let at = v.get("at_ms").and_then(Value::as_u64).unwrap_or(0);
            if now.saturating_sub(at) > STALE_MS {
                continue;
            }
            for (d, n) in v.get("counts").and_then(Value::as_object).into_iter().flatten() {
                let n = n.as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or(0);
                *others.entry(d.clone()).or_default() += n;
            }
        }
        rt.state.routing.set_remote_inflight(others, Instant::now());
    }
}

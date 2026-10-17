//! Health state the node keeps beyond its memory: manual disables in the store, so they survive a
//! restart, and the health of deployments and keys following each configuration change.

use antirion_core::UtcMicros;
use antirion_resilience::ConfigChange;
use serde_json::{Value, json};

use crate::{Runtime, Snapshot};

const DISABLES_KEY: &str = "health_disables";

impl Runtime {
    /// Writes the manual disables to the store.
    pub async fn persist_disables(&self) {
        let list: Vec<Value> = self
            .state
            .health
            .disables()
            .into_iter()
            .map(|(d, until)| json!({"deployment": d, "until": until.map(UtcMicros::to_rfc3339)}))
            .collect();
        let bytes = serde_json::to_vec(&list).unwrap_or_default();
        if let Err(e) = self.store.put_value(DISABLES_KEY, bytes).await {
            tracing::warn!(error = %e, "manual disables could not be stored");
        }
    }

    /// Restores the manual disables a previous run stored.
    pub async fn restore_disables(&self) {
        let Ok(bytes) = self.store.get_or_init(DISABLES_KEY, b"[]".to_vec()).await else {
            return;
        };
        let list: Vec<Value> = serde_json::from_slice(&bytes).unwrap_or_default();
        for item in list {
            let Some(d) = item.get("deployment").and_then(Value::as_str) else {
                continue;
            };
            let until = item
                .get("until")
                .and_then(Value::as_str)
                .and_then(|s| UtcMicros::parse_rfc3339(s).ok());
            self.state.health.restore_disable(d, until);
        }
    }

    /// Brings the health state in line with a generation that replaced `old`.
    pub(crate) fn health_follows(&self, old: &Snapshot, new: &Snapshot) {
        self.state.health.follow(&config_change(old, new), self.now());
    }
}

/// The written form of a provider key's secret: its reference, or the value as stored.
fn key_source<'a>(snap: &'a Snapshot, provider: &str, key: &str) -> Option<&'a Value> {
    let auth = snap.config.doc.get("providers")?.get(provider)?.get("auth")?;
    if key == antirion_providers::SINGLE_KEY
        && let Some(single) = auth.get("api_key")
    {
        return Some(single);
    }
    auth.get("api_keys")?
        .as_array()?
        .iter()
        .find(|k| k.get("name").and_then(Value::as_str) == Some(key))
        .and_then(|k| k.get("key"))
}

/// What a configuration change means for health: deployments removed or redefined (another
/// provider, base URL or upstream model), and keys whose secret or reference changed or that left
/// their provider.
fn config_change(old: &Snapshot, new: &Snapshot) -> ConfigChange {
    let (before, after) = (&old.providers, &new.providers);
    let mut change = ConfigChange::default();
    for (name, d) in &before.deployments {
        match after.deployments.get(name) {
            None => change.removed.push(name.clone()),
            Some(n)
                if n.provider.name != d.provider.name
                    || n.provider.base_url != d.provider.base_url
                    || n.model != d.model =>
            {
                change.redefined.push((name.clone(), n.provider.name.clone()));
            }
            Some(_) => {}
        }
    }
    for (pname, p) in &before.providers {
        let next = after.providers.get(pname);
        for k in &p.keys {
            let kept = next
                .and_then(|n| n.keys.iter().find(|x| x.name == k.name))
                .is_some_and(|n| {
                    n.value == k.value && key_source(old, pname, &k.name) == key_source(new, pname, &k.name)
                });
            if kept {
                continue;
            }
            let deployments = before
                .deployments
                .values()
                .filter(|d| d.provider.name == *pname)
                .map(|d| d.name.clone())
                .collect();
            change.rekeyed.push((pname.clone(), k.name.clone(), deployments));
        }
    }
    change
}

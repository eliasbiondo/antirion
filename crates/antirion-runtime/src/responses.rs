//! Responses state kept by the gateway: responses it stored for translated cells, and which
//! deployment created each native response id. Both expire after `api.responses.ttl` on the gateway
//! clock and are scoped to their project. In cluster mode both are also written to the store before
//! the response reaches the client, so any node resolves them; the node's map is a cache.

use std::collections::HashMap;
use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_store::StoreError;
use antirion_store::pg::SharedResponse;
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::Runtime;

/// `api.responses.ttl` of a document.
#[must_use]
pub fn ttl_of(doc: &Value) -> Duration {
    doc.pointer("/api/responses/ttl")
        .and_then(Value::as_str)
        .and_then(antirion_config::scalar::parse_duration)
        .unwrap_or(Duration::from_secs(30 * 86_400))
}

/// A response the gateway stored.
#[derive(Clone, Debug)]
pub struct StoredResponse {
    pub project: String,
    /// The response this one continued.
    pub previous: Option<String>,
    /// This turn's input items (the earlier turns' are in `previous`).
    pub input: Vec<Value>,
    pub output: Vec<Value>,
    /// The Response object as the client received it.
    pub object: Value,
    pub created: UtcMicros,
}

/// The deployment that created a native response id.
#[derive(Clone, Debug)]
pub struct NativeLink {
    pub project: String,
    pub deployment: String,
    pub created: UtcMicros,
}

/// Every stored response and native link of the node.
#[derive(Debug, Default)]
pub struct ResponsesStore {
    stored: Mutex<HashMap<String, StoredResponse>>,
    native: Mutex<HashMap<String, NativeLink>>,
}

fn live(created: UtcMicros, now: UtcMicros, ttl: Duration) -> bool {
    now.since(created) < ttl
}

impl ResponsesStore {
    pub fn store(&self, id: &str, r: StoredResponse) {
        self.stored.lock().insert(id.to_owned(), r);
    }

    /// The stored response `id` of `project`, while it lives.
    #[must_use]
    pub fn get(&self, id: &str, project: &str, now: UtcMicros, ttl: Duration) -> Option<StoredResponse> {
        let mut map = self.stored.lock();
        let r = map.get(id)?;
        if !live(r.created, now, ttl) {
            map.remove(id);
            return None;
        }
        (r.project == project).then(|| r.clone())
    }

    /// Deletes a stored response of `project`; whether there was one.
    pub fn delete(&self, id: &str, project: &str) -> bool {
        let mut map = self.stored.lock();
        if map.get(id).is_some_and(|r| r.project == project) {
            map.remove(id);
            return true;
        }
        false
    }

    pub fn link(&self, id: &str, link: NativeLink) {
        self.native.lock().insert(id.to_owned(), link);
    }

    /// The deployment that created native response `id` in `project`, while the link lives.
    #[must_use]
    pub fn native(&self, id: &str, project: &str, now: UtcMicros, ttl: Duration) -> Option<NativeLink> {
        let mut map = self.native.lock();
        let l = map.get(id)?;
        if !live(l.created, now, ttl) {
            map.remove(id);
            return None;
        }
        (l.project == project).then(|| l.clone())
    }

    pub fn clear(&self) {
        self.stored.lock().clear();
        self.native.lock().clear();
    }

    /// Forgets which deployment created each native response id.
    pub fn clear_links(&self) {
        self.native.lock().clear();
    }
}

impl StoredResponse {
    fn to_json(&self) -> String {
        json!({
            "previous": self.previous,
            "input": self.input,
            "output": self.output,
            "object": self.object,
        })
        .to_string()
    }

    fn from_shared(r: SharedResponse) -> Option<StoredResponse> {
        let v: Value = serde_json::from_str(&r.value).ok()?;
        let list = |k: &str| v.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
        Some(StoredResponse {
            project: r.project,
            previous: v.get("previous").and_then(Value::as_str).map(str::to_owned),
            input: list("input"),
            output: list("output"),
            object: v.get("object").cloned().unwrap_or(Value::Null),
            created: UtcMicros(r.created_us),
        })
    }
}

impl Runtime {
    /// Remembers which deployment created native response `id`.
    pub async fn link_response(&self, id: &str, link: NativeLink) {
        if let Some(pg) = self.store.postgres() {
            let shared = SharedResponse {
                project: link.project.clone(),
                value: link.deployment.clone(),
                created_us: link.created.0,
            };
            if let Err(e) = pg.put_response_link(id, &shared).await {
                tracing::warn!(error = %e, "a native response id is known to this node only");
            }
        }
        self.state.responses.link(id, link);
    }

    /// The deployment that created native response `id` in `project`, while the link lives.
    pub async fn native_link(
        &self,
        id: &str,
        project: &str,
        now: UtcMicros,
        ttl: Duration,
    ) -> Result<Option<NativeLink>, StoreError> {
        if let Some(l) = self.state.responses.native(id, project, now, ttl) {
            return Ok(Some(l));
        }
        let Some(pg) = self.store.postgres() else {
            return Ok(None);
        };
        let Some(r) = pg.response_link(id).await? else {
            return Ok(None);
        };
        self.state.responses.link(
            id,
            NativeLink {
                project: r.project,
                deployment: r.value,
                created: UtcMicros(r.created_us),
            },
        );
        Ok(self.state.responses.native(id, project, now, ttl))
    }

    /// Stores a response the gateway keeps.
    pub async fn keep_response(&self, id: &str, r: StoredResponse) {
        if let Some(pg) = self.store.postgres() {
            let shared = SharedResponse {
                project: r.project.clone(),
                value: r.to_json(),
                created_us: r.created.0,
            };
            if let Err(e) = pg.put_response(id, &shared).await {
                tracing::warn!(error = %e, "a stored response is known to this node only");
            }
        }
        self.state.responses.store(id, r);
    }

    /// The stored response `id` of `project`, while it lives.
    pub async fn stored_response(
        &self,
        id: &str,
        project: &str,
        now: UtcMicros,
        ttl: Duration,
    ) -> Result<Option<StoredResponse>, StoreError> {
        if let Some(r) = self.state.responses.get(id, project, now, ttl) {
            return Ok(Some(r));
        }
        let Some(pg) = self.store.postgres() else {
            return Ok(None);
        };
        let Some(r) = pg.response(id).await?.and_then(StoredResponse::from_shared) else {
            return Ok(None);
        };
        self.state.responses.store(id, r);
        Ok(self.state.responses.get(id, project, now, ttl))
    }

    /// Deletes a stored response of `project`; whether there was one.
    pub async fn delete_stored_response(&self, id: &str, project: &str) -> Result<bool, StoreError> {
        let local = self.state.responses.delete(id, project);
        match self.store.postgres() {
            Some(pg) => Ok(pg.delete_response(id, project).await? || local),
            None => Ok(local),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_and_expiring() {
        let s = ResponsesStore::default();
        let t0 = UtcMicros(0);
        s.store(
            "resp_1",
            StoredResponse {
                project: "a".into(),
                previous: None,
                input: Vec::new(),
                output: Vec::new(),
                object: Value::Null,
                created: t0,
            },
        );
        let ttl = Duration::from_secs(60);
        assert!(s.get("resp_1", "a", UtcMicros(59_000_000), ttl).is_some());
        assert!(s.get("resp_1", "b", UtcMicros(1), ttl).is_none());
        assert!(s.get("resp_1", "a", UtcMicros(61_000_000), ttl).is_none());
        assert!(!s.delete("resp_1", "a"));
    }
}

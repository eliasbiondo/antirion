//! The playground's side of the runtime: the in-process way into this node's data plane, the keys
//! the playground minted for each operator and project (their secrets never leave the node), and the
//! deletion of playground keys once they expire.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use antirion_core::audit::{Actor, ActorKind};
use antirion_core::{RequestId, UtcMicros};
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use parking_lot::Mutex;
use serde_json::Value;

use crate::changes::Target;
use crate::{AuditContext, Mutation, Runtime};

/// The creator recorded on the keys the playground mints.
pub const CREATOR: &str = "playground";

/// How often expired playground keys are looked for.
const SWEEP_EVERY: Duration = Duration::from_secs(1);

pub type PlaneBody = BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;
pub type PlaneFuture = Pin<Box<dyn Future<Output = Result<http::Response<PlaneBody>, PlaneError>> + Send>>;

/// This node's data plane, reached without a network.
pub trait LocalPlane: Send + Sync + fmt::Debug {
    /// Sends one request through the whole data-plane lifecycle, as a client on loopback would.
    fn send(&self, req: http::Request<Bytes>) -> PlaneFuture {
        self.send_from(
            req,
            antirion_core::Peer {
                addr: std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)),
                tls: false,
                client_cert: None,
            },
        )
    }

    /// Sends one request as if it had arrived on `peer`'s connection.
    fn send_from(&self, req: http::Request<Bytes>, peer: antirion_core::Peer) -> PlaneFuture;
}

#[derive(Debug, thiserror::Error)]
pub enum PlaneError {
    #[error("the data plane is not available on this node")]
    Unavailable,
    #[error("the local exchange failed: {0}")]
    Exchange(String),
}

/// A key the playground minted, with its secret.
struct Minted {
    name: String,
    secret: String,
    expires_at: UtcMicros,
    sends: u32,
}

impl fmt::Debug for Minted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Minted")
            .field("name", &self.name)
            .field("expires_at", &self.expires_at)
            .field("sends", &self.sends)
            .finish_non_exhaustive()
    }
}

/// The playground's node state.
#[derive(Debug, Default)]
pub struct PlaygroundState {
    plane: OnceLock<Arc<dyn LocalPlane>>,
    /// By `(caller, project)`.
    keys: Mutex<HashMap<(String, String), Minted>>,
}

impl PlaygroundState {
    /// Installs the way into the data plane; the server does it once at startup.
    pub fn install(&self, plane: Arc<dyn LocalPlane>) {
        let _ = self.plane.set(plane);
    }

    #[must_use]
    pub fn plane(&self) -> Option<Arc<dyn LocalPlane>> {
        self.plane.get().cloned()
    }

    /// The name and secret of the caller's key for `project`, counting one more send, while it is
    /// valid at `now`, still in the configuration and under `max_sends`.
    pub fn take_send(
        &self,
        caller: &str,
        project: &str,
        now: UtcMicros,
        max_sends: u32,
        exists: impl Fn(&str) -> bool,
    ) -> Option<(String, String)> {
        let mut keys = self.keys.lock();
        let k = keys.get_mut(&(caller.to_owned(), project.to_owned()))?;
        if k.expires_at <= now || k.sends >= max_sends || !exists(&k.name) {
            return None;
        }
        k.sends += 1;
        Some((k.name.clone(), k.secret.clone()))
    }

    /// Keeps a newly minted key, its first send counted.
    pub fn remember(&self, caller: &str, project: &str, name: String, secret: String, expires_at: UtcMicros) {
        self.keys.lock().insert(
            (caller.to_owned(), project.to_owned()),
            Minted {
                name,
                secret,
                expires_at,
                sends: 1,
            },
        );
    }

    fn forget_expired(&self, now: UtcMicros) {
        self.keys.lock().retain(|_, k| k.expires_at > now);
    }
}

/// The audit context of what the playground itself changes.
#[must_use]
pub fn audit_context(action: &str, resource: &str, status: u16) -> AuditContext {
    AuditContext {
        actor: Actor {
            kind: ActorKind::System,
            id: Some(CREATOR.to_owned()),
        },
        action: action.to_owned(),
        resource: resource.to_owned(),
        request_id: RequestId::generate().as_str().to_owned(),
        source_ip: None,
        status_code: status,
        reason: None,
    }
}

impl Runtime {
    /// Whether the key named `name` was minted by the playground.
    #[must_use]
    pub fn is_playground_key(&self, name: &str) -> bool {
        self.gens
            .load()
            .config
            .meta
            .resources
            .get("keys")
            .and_then(|m| m.get(name))
            .and_then(|m| m.created_by.as_deref())
            == Some(CREATOR)
    }

    /// The playground keys that expired at `now`.
    fn expired_playground_keys(&self, now: UtcMicros) -> Vec<String> {
        let snap = self.gens.load();
        let Some(meta) = snap.config.meta.resources.get("keys") else {
            return Vec::new();
        };
        let Some(keys) = snap.config.doc.get("keys").and_then(Value::as_object) else {
            return Vec::new();
        };
        keys.iter()
            .filter(|(name, _)| meta.get(*name).and_then(|m| m.created_by.as_deref()) == Some(CREATOR))
            .filter(|(_, spec)| {
                spec.get("expires_at")
                    .and_then(Value::as_str)
                    .and_then(|t| UtcMicros::parse_rfc3339(t).ok())
                    .is_some_and(|t| t <= now)
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Deletes the playground keys that expired; true when there were any.
    pub async fn delete_expired_playground_keys(&self) -> bool {
        let now = self.now();
        self.state.playground.forget_expired(now);
        let expired = self.expired_playground_keys(now);
        if expired.is_empty() {
            return false;
        }
        let resource = format!("keys/{}", expired.join(","));
        let result = self
            .mutate(|state| {
                let mut doc = state.doc.clone();
                if let Some(keys) = doc.get_mut("keys").and_then(Value::as_object_mut) {
                    for name in &expired {
                        keys.remove(name);
                    }
                }
                Ok::<_, ()>(Mutation {
                    candidate: doc,
                    writer: "api".to_owned(),
                    written: expired
                        .iter()
                        .map(|n| Target::Resource("keys", n.clone()))
                        .collect(),
                    credentials: Vec::new(),
                    meta_edits: Vec::new(),
                    dry_run: false,
                    source: CREATOR.to_owned(),
                    audit: Some(audit_context("keys.delete", &resource, 204)),
                })
            })
            .await;
        if let Err(e) = result {
            tracing::debug!(error = ?e, "expired playground keys could not be deleted");
        }
        true
    }

    /// Deletes expired playground keys until the node stops.
    pub async fn sweep_playground_keys(self: Arc<Self>) {
        loop {
            tokio::select! {
                () = self.shutdown.cancelled() => return,
                () = tokio::time::sleep(SWEEP_EVERY) => {}
            }
            self.delete_expired_playground_keys().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_reused_until_it_expires_or_is_used_up() {
        let s = PlaygroundState::default();
        let now = UtcMicros(1_000);
        s.remember("alice", "p", "k1".into(), "sk".into(), UtcMicros(2_000));
        assert_eq!(
            s.take_send("alice", "p", now, 3, |_| true),
            Some(("k1".into(), "sk".into()))
        );
        assert!(s.take_send("bob", "p", now, 3, |_| true).is_none());
        assert!(s.take_send("alice", "p", now, 3, |_| false).is_none());
        assert!(s.take_send("alice", "p", now, 3, |_| true).is_some());
        assert!(
            s.take_send("alice", "p", now, 3, |_| true).is_none(),
            "three sends at most"
        );
        s.remember("alice", "q", "k2".into(), "sk2".into(), UtcMicros(1_500));
        assert!(s.take_send("alice", "q", UtcMicros(1_500), 9, |_| true).is_none());
        s.forget_expired(UtcMicros(1_600));
        assert!(!format!("{:?}", s).contains("sk2"));
    }
}

//! Provider credential rotation: a rotated credential's predecessor stays as a standby key until
//! its overlap ends, then a sweep removes it as a configuration change of its own; and the
//! one-time notice that a credential is older than its provider's `credential_rotation_max_age`.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::audit::{Actor, ActorKind};
use antirion_core::{RequestId, UtcMicros};
use antirion_store::{PendingRequest, RequestFilter, StoreError};
use serde_json::{Value, json};

use crate::Runtime;
use crate::changes::Target;
use crate::commit::{AuditContext, Mutation};

/// The store kind of a scheduled retirement.
const RETIRE: &str = "retire";
/// The store kind that records a rotation-age notice was sent.
const DUE: &str = "rotation_due";
const SWEEP_EVERY: Duration = Duration::from_secs(1);
/// The actor of the changes the sweep makes.
const SWEEPER: &str = "credential_rotation";

fn system_audit(action: &str, resource: &str) -> AuditContext {
    AuditContext {
        actor: Actor {
            kind: ActorKind::System,
            id: Some(SWEEPER.to_owned()),
        },
        action: action.to_owned(),
        resource: resource.to_owned(),
        request_id: RequestId::generate().as_str().to_owned(),
        source_ip: None,
        status_code: 200,
        reason: None,
    }
}

/// The provider's `api_keys` without the entry named `entry`.
fn without(doc: &Value, provider: &str, entry: &str) -> Value {
    let mut doc = doc.clone();
    if let Some(auth) = doc
        .pointer_mut(&format!("/providers/{provider}/auth"))
        .and_then(Value::as_object_mut)
    {
        let empty = match auth.get_mut("api_keys").and_then(Value::as_array_mut) {
            Some(keys) => {
                keys.retain(|k| k.get("name").and_then(Value::as_str) != Some(entry));
                keys.is_empty()
            }
            None => false,
        };
        if empty {
            auth.remove("api_keys");
        }
    }
    doc
}

impl Runtime {
    /// Schedules the removal of provider key `entry` of `provider` at `at`.
    pub async fn schedule_retirement(
        &self,
        provider: &str,
        entry: &str,
        at: UtcMicros,
    ) -> Result<(), StoreError> {
        let id = format!("{provider}/{entry}");
        self.store
            .insert_request(PendingRequest {
                kind: RETIRE.to_owned(),
                id,
                subject: provider.to_owned(),
                state: "pending".to_owned(),
                author: SWEEPER.to_owned(),
                created_us: at.0,
                version: 0,
                body: json!({"provider": provider, "entry": entry, "at_us": at.0}).to_string(),
            })
            .await
            .map(|_| ())
    }

    /// Removes the standby keys whose overlap has ended. Each is claimed with a compare-and-set
    /// first, so one node of a cluster makes the change.
    pub async fn retire_due_credentials(&self) {
        let now = self.now();
        let Ok(pending) = self
            .store
            .requests(
                RETIRE,
                &RequestFilter {
                    states: vec!["pending".to_owned()],
                    limit: 1000,
                    ..RequestFilter::default()
                },
            )
            .await
        else {
            return;
        };
        for job in pending.into_iter().filter(|j| j.created_us <= now.0) {
            let Ok(true) = self
                .store
                .update_request(RETIRE, &job.id, job.version, "done", &job.body)
                .await
            else {
                continue;
            };
            let body: Value = serde_json::from_str(&job.body).unwrap_or(Value::Null);
            let (Some(provider), Some(entry)) = (
                body.get("provider").and_then(Value::as_str),
                body.get("entry").and_then(Value::as_str),
            ) else {
                continue;
            };
            let resource = format!("providers/{provider}");
            let result = self
                .mutate(|state| {
                    Ok::<_, ()>(Mutation {
                        candidate: without(&state.doc, provider, entry),
                        writer: "api".to_owned(),
                        written: vec![Target::Resource("providers", provider.to_owned())],
                        credentials: Vec::new(),
                        meta_edits: Vec::new(),
                        dry_run: false,
                        source: "api".to_owned(),
                        audit: Some(system_audit("POST providers:retire_credential", &resource)),
                    })
                })
                .await;
            if let Err(e) = result {
                tracing::warn!(provider, error = ?e, "a retired provider credential could not be removed");
            }
        }
    }

    /// Sends `provider.credential_rotation_due` once for each credential older than its
    /// provider's `credential_rotation_max_age` since it was last set.
    pub async fn notify_rotation_due(&self) {
        let snap = self.gens.load();
        let now = self.now();
        let Some(providers) = snap.config.doc.get("providers").and_then(Value::as_object) else {
            return;
        };
        for (name, spec) in providers {
            let Some(max_age) = spec
                .get("credential_rotation_max_age")
                .and_then(Value::as_str)
                .and_then(antirion_config::scalar::parse_duration)
            else {
                continue;
            };
            let Some(meta) = snap
                .config
                .meta
                .resources
                .get("providers")
                .and_then(|m| m.get(name))
            else {
                continue;
            };
            for (credential, at) in &meta.credentials_rotated_at {
                let Ok(set) = UtcMicros::parse_rfc3339(at) else {
                    continue;
                };
                if now.since(set) <= max_age {
                    continue;
                }
                let inserted = self
                    .store
                    .insert_request(PendingRequest {
                        kind: DUE.to_owned(),
                        id: format!("{name}/{credential}/{}", set.0),
                        subject: name.clone(),
                        state: "sent".to_owned(),
                        author: SWEEPER.to_owned(),
                        created_us: now.0,
                        version: 0,
                        body: "{}".to_owned(),
                    })
                    .await;
                if matches!(inserted, Ok(true)) {
                    self.send_alert(
                        &snap,
                        None,
                        "provider.credential_rotation_due",
                        json!({"provider": name, "credential": credential, "rotated_at": at,
                               "max_age": spec.get("credential_rotation_max_age")}),
                    );
                }
            }
        }
    }

    /// Retires standby credentials and sends rotation-age notices until the node stops.
    pub async fn sweep_rotations(self: Arc<Self>) {
        loop {
            tokio::select! {
                () = self.shutdown.cancelled() => return,
                () = tokio::time::sleep(SWEEP_EVERY) => {}
            }
            self.retire_due_credentials().await;
            self.notify_rotation_due().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retiring_removes_one_entry_and_an_emptied_list() {
        let doc = json!({"providers": {"p": {"auth": {"api_key": "new", "api_keys": [{"name": "old", "key": "k"}]}}}});
        let after = without(&doc, "p", "old");
        assert_eq!(
            after.pointer("/providers/p/auth"),
            Some(&json!({"api_key": "new"}))
        );
        let two = json!({"providers": {"p": {"auth": {"api_keys": [{"name": "a", "key": "1"}, {"name": "b", "key": "2"}]}}}});
        let after = without(&two, "p", "b");
        assert_eq!(
            after.pointer("/providers/p/auth/api_keys/0/name"),
            Some(&json!("a"))
        );
        assert!(after.pointer("/providers/p/auth/api_keys/1").is_none());
    }
}

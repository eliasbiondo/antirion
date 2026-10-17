//! Scheduled deletion of an organization: the schedule is a row of the shared store; once its grace
//! has passed one node claims it, removes the organization and everything below it from the
//! configuration in one generation, then erases the rows the tenant left in the store and the
//! request log. Every node then hides the erased tenant's lines it still holds in memory.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use antirion_core::audit::{Actor, ActorKind};
use antirion_core::{RequestId, UtcMicros};
use antirion_store::logs::{Arg, LogColumn, LogFilter};
use antirion_store::{RequestFilter, StoreError, Tenant};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::changes::Target;
use crate::commit::{AuditContext, CommitError, Mutation};
use crate::{ConfigState, Runtime};

/// The store kind of a schedule; its id is the organization's name.
pub const KIND: &str = "organization_deletion";
pub const SCHEDULED: &str = "scheduled";
pub const CANCELLED: &str = "cancelled";
const DELETING: &str = "deleting";
pub const DELETED: &str = "deleted";
pub const FAILED: &str = "failed";
/// The actor of the deletion's change.
const ACTOR: &str = "organization_deletion";
/// How long the erasure waits after the configuration change, so records other nodes queued
/// before they stopped serving the tenant's keys are written and erased with the rest.
const ERASE_DELAY: Duration = Duration::from_secs(3);

/// A schedule as stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub organization: String,
    pub delete_at_us: i64,
    /// The organization's `enabled` before the schedule disabled it (absent: `None`).
    pub enabled_before: Option<bool>,
    pub scheduled_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at_us: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The names below an organization: its teams, their projects and the projects' keys.
#[must_use]
pub fn subtree(doc: &Value, organization: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let default = antirion_config::model::DEFAULT_TENANT;
    let children = |collection: &str, field: &str, parents: &BTreeSet<String>| -> Vec<String> {
        doc.get(collection)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(_, s)| {
                let parent = s.get(field).and_then(Value::as_str);
                let parent = if field == "project" {
                    parent.or(Some(default))
                } else {
                    parent
                };
                parent.is_some_and(|p| parents.contains(p))
            })
            .map(|(n, _)| n.clone())
            .collect()
    };
    let teams = children(
        "teams",
        "organization",
        &BTreeSet::from([organization.to_owned()]),
    );
    let projects = children("projects", "team", &teams.iter().cloned().collect());
    let keys = children("keys", "project", &projects.iter().cloned().collect());
    (teams, projects, keys)
}

/// The document without the organization and its subtree: role bindings scoped inside it are
/// dropped from users, and a primary team inside it is cleared.
#[must_use]
pub fn without(doc: &Value, organization: &str) -> (Value, Vec<Target>) {
    let (teams, projects, keys) = subtree(doc, organization);
    let mut out = doc.clone();
    let mut written = vec![Target::Resource("organizations", organization.to_owned())];
    for (collection, names) in [("teams", &teams), ("projects", &projects), ("keys", &keys)] {
        if let Some(map) = out.get_mut(collection).and_then(Value::as_object_mut) {
            for n in names {
                map.remove(n);
            }
        }
        written.extend(names.iter().map(|n| Target::Resource(collection, n.clone())));
    }
    if let Some(map) = out.get_mut("organizations").and_then(Value::as_object_mut) {
        map.remove(organization);
    }
    let scopes: BTreeSet<String> = std::iter::once(format!("organization/{organization}"))
        .chain(teams.iter().map(|t| format!("team/{t}")))
        .chain(projects.iter().map(|p| format!("project/{p}")))
        .collect();
    for (name, user) in out
        .get_mut("users")
        .and_then(Value::as_object_mut)
        .into_iter()
        .flatten()
    {
        let mut changed = false;
        if let Some(bindings) = user.get_mut("role_bindings").and_then(Value::as_array_mut) {
            let before = bindings.len();
            bindings.retain(|b| {
                b.get("scope")
                    .and_then(Value::as_str)
                    .is_none_or(|s| !scopes.contains(s))
            });
            changed |= bindings.len() != before;
        }
        let primary = user.get("primary_team").and_then(Value::as_str);
        if primary.is_some_and(|t| teams.iter().any(|x| x == t))
            && let Some(obj) = user.as_object_mut()
        {
            obj.remove("primary_team");
            changed = true;
        }
        if changed {
            written.push(Target::Resource("users", name.clone()));
        }
    }
    (out, written)
}

/// The rows a deletion erases, read from the configuration before it.
fn tenant_of(state: &ConfigState, organization: &str) -> Tenant {
    let (teams, projects, keys) = subtree(&state.doc, organization);
    let key_uids = keys
        .iter()
        .filter_map(|k| {
            state
                .meta
                .resources
                .get("keys")
                .and_then(|m| m.get(k))
                .map(|m| m.uid.to_hex())
        })
        .collect();
    Tenant {
        organization: organization.to_owned(),
        teams,
        projects,
        keys,
        key_uids,
    }
}

/// Why a deletion did not complete.
#[derive(Debug, thiserror::Error)]
pub enum DeletionError {
    #[error("the store failed: {0}")]
    Store(#[from] StoreError),
    #[error("the configuration without the organization is invalid: {0}")]
    Invalid(String),
}

/// What every node remembers of erased tenants: lines of the organization admitted before its
/// erasure are hidden from spend reads.
#[derive(Debug, Default)]
pub struct Erased {
    at: parking_lot::RwLock<HashMap<String, i64>>,
}

impl Erased {
    /// Whether a line of `organization` admitted at `admitted` belongs to an erased tenant.
    #[must_use]
    pub fn hides(&self, organization: &str, admitted: UtcMicros) -> bool {
        let at = self.at.read();
        !at.is_empty() && at.get(organization).is_some_and(|t| admitted.0 < *t)
    }

    fn learn(&self, organization: &str, at_us: i64) -> bool {
        let mut at = self.at.write();
        let known = at.get(organization).is_some_and(|t| *t >= at_us);
        if !known {
            at.insert(organization.to_owned(), at_us);
        }
        !known
    }

    /// Forgets every erasure (test mode).
    pub fn clear(&self) {
        self.at.write().clear();
    }
}

impl Runtime {
    /// Deletes every organization whose grace has passed. Each schedule is claimed with a
    /// compare-and-set, so one node of a cluster deletes it.
    pub async fn delete_due_organizations(&self) {
        let Ok(rows) = self
            .store
            .requests(
                KIND,
                &RequestFilter {
                    states: vec![SCHEDULED.to_owned()],
                    limit: 1000,
                    ..RequestFilter::default()
                },
            )
            .await
        else {
            return;
        };
        let now = self.now();
        for row in rows {
            let Ok(mut schedule) = serde_json::from_str::<Schedule>(&row.body) else {
                continue;
            };
            if schedule.delete_at_us > now.0 {
                continue;
            }
            let Ok(true) = self
                .store
                .update_request(KIND, &row.id, row.version, DELETING, &row.body)
                .await
            else {
                continue;
            };
            let claimed = row.version + 1;
            let (state, note) = match self.delete_organization(&schedule).await {
                Ok(()) => {
                    schedule.deleted_at_us = Some(self.now().0);
                    (DELETED, None)
                }
                Err(DeletionError::Store(e)) => {
                    tracing::warn!(organization = %schedule.organization, error = %e, "a scheduled organization deletion will be retried");
                    (SCHEDULED, None)
                }
                Err(e) => {
                    tracing::error!(organization = %schedule.organization, error = %e, "a scheduled organization deletion failed");
                    (FAILED, Some(e.to_string()))
                }
            };
            schedule.error = note;
            let body = serde_json::to_string(&schedule).unwrap_or_else(|_| row.body.clone());
            if let Err(e) = self
                .store
                .update_request(KIND, &row.id, claimed, state, &body)
                .await
            {
                tracing::warn!(organization = %schedule.organization, error = %e, "the state of an organization deletion could not be stored");
            }
        }
    }

    async fn delete_organization(&self, schedule: &Schedule) -> Result<(), DeletionError> {
        let org = schedule.organization.as_str();
        let mut tenant = None;
        let audit = AuditContext {
            actor: Actor {
                kind: ActorKind::System,
                id: Some(ACTOR.to_owned()),
            },
            action: "organizations.delete".to_owned(),
            resource: format!("organizations/{org}"),
            request_id: RequestId::generate().as_str().to_owned(),
            source_ip: None,
            status_code: 204,
            reason: Some(format!(
                "scheduled deletion requested by {}",
                schedule.scheduled_by
            )),
        };
        let result = self
            .mutate(|state| {
                tenant = Some(tenant_of(state, org));
                if !state
                    .doc
                    .get("organizations")
                    .and_then(Value::as_object)
                    .is_some_and(|m| m.contains_key(org))
                {
                    return Err(());
                }
                let (candidate, written) = without(&state.doc, org);
                Ok(Mutation {
                    candidate,
                    writer: "api".to_owned(),
                    written,
                    credentials: Vec::new(),
                    meta_edits: Vec::new(),
                    dry_run: false,
                    source: "api".to_owned(),
                    audit: Some(audit),
                })
            })
            .await;
        match result {
            Ok(_) | Err(CommitError::Rejected(())) => {}
            Err(CommitError::Store(e)) => return Err(e.into()),
            Err(CommitError::Invalid(details)) => {
                let text = details
                    .iter()
                    .map(|d| format!("{}: {}", d.path, d.message))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(DeletionError::Invalid(text));
            }
            Err(CommitError::Builder) => return Err(DeletionError::Store(StoreError::Unavailable)),
        }
        let tenant = tenant.unwrap_or_else(|| Tenant {
            organization: org.to_owned(),
            ..Tenant::default()
        });
        tokio::select! {
            () = self.shutdown.cancelled() => return Err(DeletionError::Store(StoreError::Closed)),
            () = tokio::time::sleep(ERASE_DELAY) => {}
        }
        self.erase_tenant(&tenant).await?;
        Ok(())
    }

    /// Erases what `t` left in the store and the request log, and hides its lines on this node.
    pub async fn erase_tenant(&self, t: &Tenant) -> Result<u64, StoreError> {
        self.flush_ledger().await;
        let mut n = self.store.erase_tenant(t).await?;
        n += self
            .erase_logs(LogFilter {
                any_of: vec![(LogColumn::Organization, vec![Arg::Text(t.organization.clone())])],
                ..LogFilter::default()
            })
            .await?;
        self.state.cache.epochs.bump("organization", &t.organization);
        self.persist_cache_epochs().await;
        self.state.operations.erased.learn(&t.organization, self.now().0);
        Ok(n)
    }

    /// Learns the erasures other nodes made. A node that learns one writes what it still holds of the
    /// tenant's ledger and removes it again, so a line it settled before the erasure but wrote after
    /// it does not stay in the store.
    pub async fn learn_erasures(&self) {
        let Ok(rows) = self
            .store
            .requests(
                KIND,
                &RequestFilter {
                    states: vec![DELETED.to_owned()],
                    limit: 10_000,
                    ..RequestFilter::default()
                },
            )
            .await
        else {
            return;
        };
        for row in rows {
            if let Ok(Schedule {
                organization,
                deleted_at_us: Some(at),
                ..
            }) = serde_json::from_str::<Schedule>(&row.body)
                && self.state.operations.erased.learn(&organization, at)
            {
                self.flush_ledger().await;
                if let Err(e) = self.store.erase_ledger(&organization, at).await {
                    tracing::warn!(organization, error = %e, "an erased tenant's ledger lines could not be removed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn doc() -> Value {
        json!({
            "organizations": {"acme": {}, "other": {}},
            "teams": {"red": {"organization": "acme"}, "blue": {"organization": "other"}},
            "projects": {"web": {"team": "red"}, "api": {"team": "blue"}},
            "keys": {"k1": {"project": "web"}, "k2": {"project": "api"}, "k3": {}},
            "users": {
                "ana": {"role_bindings": [
                    {"role": "viewer", "scope": "organization/acme"},
                    {"role": "viewer", "scope": "project/api"}
                ], "primary_team": "red"},
                "bob": {"role_bindings": [{"role": "viewer", "scope": "team/blue"}]}
            }
        })
    }

    #[test]
    fn the_subtree_follows_the_hierarchy() {
        let (teams, projects, keys) = subtree(&doc(), "acme");
        assert_eq!(
            (teams, projects, keys),
            (vec!["red".into()], vec!["web".into()], vec!["k1".into()])
        );
    }

    #[test]
    fn removal_keeps_every_other_tenant() {
        let (after, written) = without(&doc(), "acme");
        assert_eq!(after["organizations"], json!({"other": {}}));
        assert_eq!(after["teams"], json!({"blue": {"organization": "other"}}));
        assert_eq!(after["projects"], json!({"api": {"team": "blue"}}));
        assert_eq!(after["keys"], json!({"k2": {"project": "api"}, "k3": {}}));
        assert_eq!(
            after["users"]["ana"],
            json!({"role_bindings": [{"role": "viewer", "scope": "project/api"}]})
        );
        assert_eq!(after["users"]["bob"], doc()["users"]["bob"]);
        assert!(written.contains(&Target::Resource("users", "ana".into())));
        assert!(!written.contains(&Target::Resource("users", "bob".into())));
    }

    #[test]
    fn erased_lines_are_hidden_until_the_erasure_time_only() {
        let e = Erased::default();
        assert!(!e.hides("acme", UtcMicros(5)));
        assert!(e.learn("acme", 10));
        assert!(!e.learn("acme", 9));
        assert!(e.hides("acme", UtcMicros(5)));
        assert!(!e.hides("acme", UtcMicros(10)));
        assert!(!e.hides("other", UtcMicros(5)));
    }
}

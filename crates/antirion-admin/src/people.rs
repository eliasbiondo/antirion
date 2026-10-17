//! What the API keys and the Teams and users screens read beyond configuration: a key's type,
//! status, team and last use; a user's last sign-in, second factors, last activity, state and
//! flag; and the filters and counts of both lists.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use antirion_config::model::{CollectionDef, DEFAULT_TENANT};
use antirion_core::{ResourceUid, UtcMicros};
use antirion_runtime::people::marks::LastLogin;
use antirion_runtime::state::ResourceMeta;
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::error::AdminError;

/// A key living at most this long is ephemeral; one expiring within it is `expiring`.
const SHORT: Duration = Duration::from_secs(7 * 86_400);
const KEY_TYPES: [&str; 3] = ["user", "service", "ephemeral"];
const KEY_STATUSES: [&str; 5] = ["active", "disabled", "rotating", "expiring", "expired"];
const USER_STATES: [&str; 3] = ["active", "invited", "disabled"];

/// The facts of the people screens that live outside the configuration, read once per call.
#[derive(Debug, Default)]
pub(crate) struct Facts {
    now: UtcMicros,
    last_used: HashMap<ResourceUid, UtcMicros>,
    logins: HashMap<String, LastLogin>,
    factors: HashMap<String, BTreeSet<String>>,
    activity: HashMap<String, i64>,
    invited: HashSet<String>,
    /// Open anomaly flags by key name.
    open_flags: HashMap<String, u64>,
}

fn ts(t: UtcMicros) -> Value {
    Value::from(t.to_rfc3339_seconds())
}

fn time(spec: &Value, field: &str) -> Option<UtcMicros> {
    spec.get(field)
        .and_then(Value::as_str)
        .and_then(|s| UtcMicros::parse_rfc3339(s).ok())
}

impl Facts {
    /// The facts `collection` needs; `None` for the other collections.
    pub(crate) async fn load(call: &Call, collection: &str) -> Option<Facts> {
        let rt = &call.rt;
        let mut facts = Facts {
            now: rt.now(),
            ..Facts::default()
        };
        match collection {
            "keys" => {
                facts.last_used = rt.keys_last_used().await;
                facts.open_flags = rt.open_flags_by_key().await;
            }
            "users" => {
                facts.last_used = rt.keys_last_used().await;
                facts.open_flags = rt.open_flags_by_key().await;
                facts.logins = rt.last_logins().await;
                facts.factors = rt.factor_kinds().await;
                facts.activity = rt.store.user_activity().await.unwrap_or_default();
                facts.invited = rt
                    .invitations()
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|i| i.user)
                    .collect();
            }
            _ => return None,
        }
        Some(facts)
    }

    fn key_last_used(&self, meta: Option<&ResourceMeta>) -> Option<UtcMicros> {
        meta.and_then(|m| self.last_used.get(&m.uid).copied())
    }

    /// `service` for a key a service account owns, `ephemeral` for a playground key or one that
    /// lives at most seven days, `service` for any other key without an owner, else `user`.
    fn key_type(&self, doc: &Value, spec: &Value, meta: Option<&ResourceMeta>) -> &'static str {
        let owner = spec.get("owner").and_then(Value::as_str);
        let service_account = owner.is_some_and(|o| {
            doc.pointer(&format!("/users/{o}/kind")).and_then(Value::as_str) == Some("service")
        });
        let created = meta.and_then(|m| UtcMicros::parse_rfc3339(&m.created_at).ok());
        let short = matches!((created, time(spec, "expires_at")), (Some(c), Some(e)) if e.since(c) <= SHORT);
        if service_account {
            "service"
        } else if short || meta.and_then(|m| m.created_by.as_deref()) == Some("playground") {
            "ephemeral"
        } else if owner.is_none() {
            "service"
        } else {
            "user"
        }
    }

    fn key_status(&self, spec: &Value, meta: Option<&ResourceMeta>) -> &'static str {
        let expires = time(spec, "expires_at");
        let rotating = meta
            .and_then(|m| m.credential.as_ref())
            .and_then(|c| c.previous.as_ref())
            .and_then(|p| UtcMicros::parse_rfc3339(&p.valid_until).ok())
            .is_some_and(|until| until > self.now);
        if spec.get("enabled") == Some(&Value::Bool(false)) {
            "disabled"
        } else if expires.is_some_and(|e| e <= self.now) {
            "expired"
        } else if rotating {
            "rotating"
        } else if expires.is_some_and(|e| e.since(self.now) <= SHORT) {
            "expiring"
        } else {
            "active"
        }
    }

    fn user_last_active(&self, call: &Call, user: &str) -> Option<UtcMicros> {
        let login = self
            .logins
            .get(user)
            .and_then(|l| UtcMicros::parse_rfc3339(&l.at).ok());
        let used = self.activity.get(user).map(|t| UtcMicros(*t));
        let snap = &call.snap.config;
        let metas = snap.meta.resources.get("keys");
        let keys = snap
            .doc
            .get("keys")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(_, k)| k.get("owner").and_then(Value::as_str) == Some(user))
            .filter_map(|(name, _)| self.key_last_used(metas.and_then(|m| m.get(name))))
            .max();
        [login, used, keys].into_iter().flatten().max()
    }

    fn user_state(&self, user: &str, spec: &Value) -> &'static str {
        if spec.get("enabled") != Some(&Value::Bool(false)) {
            "active"
        } else if self.invited.contains(user) {
            "invited"
        } else {
            "disabled"
        }
    }

    /// Whether a key `user` owns has an open anomaly flag.
    fn flagged(&self, call: &Call, user: &str) -> bool {
        call.snap
            .config
            .doc
            .get("keys")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .any(|(name, k)| {
                k.get("owner").and_then(Value::as_str) == Some(user)
                    && self.open_flags.get(name).is_some_and(|n| *n > 0)
            })
    }

    /// Adds the read-only facts of a key or a user to its representation.
    pub(crate) fn decorate(
        &self,
        call: &Call,
        collection: &str,
        name: &str,
        body: &mut Value,
        meta: Option<&ResourceMeta>,
    ) {
        let spec = body.get("spec").cloned().unwrap_or(Value::Null);
        let mut add = Map::new();
        match collection {
            "keys" => {
                let project = spec
                    .get("project")
                    .and_then(Value::as_str)
                    .unwrap_or(DEFAULT_TENANT);
                let scopes = call.snap.identity.project_scopes(project);
                let named = |i: usize| {
                    scopes
                        .get(i)
                        .map(|s| s.to_string().split_once('/').map(|(_, n)| n.to_owned()))
                };
                add.insert(
                    "last_used_at".into(),
                    self.key_last_used(meta).map_or(Value::Null, ts),
                );
                add.insert(
                    "type".into(),
                    Value::from(self.key_type(&call.snap.config.doc, &spec, meta)),
                );
                add.insert("status".into(), Value::from(self.key_status(&spec, meta)));
                add.insert("organization".into(), Value::from(named(1).flatten()));
                add.insert("team".into(), Value::from(named(2).flatten()));
                add.insert(
                    "open_anomalies".into(),
                    Value::from(self.open_flags.get(name).copied().unwrap_or(0)),
                );
            }
            "users" => {
                let login = self.logins.get(name);
                let mut mfa: Vec<String> = self.factors.get(name).into_iter().flatten().cloned().collect();
                if login.is_some_and(LastLogin::idp_mfa) {
                    mfa.push("idp".to_owned());
                }
                add.insert("last_login_at".into(), Value::from(login.map(|l| l.at.clone())));
                add.insert(
                    "last_login_method".into(),
                    Value::from(login.map(|l| l.method.clone())),
                );
                add.insert(
                    "last_login_provider".into(),
                    Value::from(login.and_then(|l| l.provider.clone())),
                );
                add.insert("mfa_methods".into(), json!(mfa));
                add.insert(
                    "last_active_at".into(),
                    self.user_last_active(call, name).map_or(Value::Null, ts),
                );
                add.insert("state".into(), Value::from(self.user_state(name, &spec)));
                add.insert("flagged".into(), Value::Bool(self.flagged(call, name)));
            }
            _ => return,
        }
        if let Some(obj) = body.get_mut("metadata").and_then(Value::as_object_mut) {
            obj.extend(add);
        }
    }
}

/// What a key or user list selects beyond pagination.
#[derive(Debug, Default)]
pub(crate) struct Filter {
    before: Option<UtcMicros>,
    owners: Vec<String>,
    teams: Vec<String>,
    projects: Vec<String>,
    roles: Vec<String>,
    types: Vec<String>,
    statuses: Vec<String>,
    q: Option<String>,
}

fn all(call: &Call, name: &str) -> Vec<String> {
    call.query
        .iter()
        .filter(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
        .collect()
}

fn one_of(values: Vec<String>, allowed: &[&str], name: &str) -> Result<Vec<String>, AdminError> {
    match values.iter().find(|v| !allowed.contains(&v.as_str())) {
        Some(bad) => Err(AdminError::invalid_parameter(format!(
            "{name} {bad:?} is not one of {}",
            allowed.join(", ")
        ))),
        None => Ok(values),
    }
}

fn contains(haystack: Option<&str>, needle: &str) -> bool {
    haystack.is_some_and(|h| h.to_lowercase().contains(needle))
}

impl Filter {
    /// The filters of `GET /keys` or `GET /users`; `None` for other collections.
    pub(crate) fn from_call(call: &Call, c: &CollectionDef) -> Result<Option<Filter>, AdminError> {
        let (since, statuses, allowed): (&str, &[&str], &[&str]) = match c.name {
            "keys" => (
                "last_used_before",
                &KEY_STATUSES,
                &[
                    "limit",
                    "cursor",
                    "last_used_before",
                    "owner",
                    "team",
                    "project",
                    "type",
                    "status",
                    "q",
                ],
            ),
            "users" => (
                "last_active_before",
                &USER_STATES,
                &[
                    "limit",
                    "cursor",
                    "last_active_before",
                    "state",
                    "role",
                    "team",
                    "q",
                ],
            ),
            _ => return Ok(None),
        };
        call.allow_params(allowed)?;
        let before = call
            .param(since)
            .map(|s| {
                UtcMicros::parse_rfc3339(s)
                    .map_err(|_| AdminError::invalid_parameter(format!("{since} must be an RFC 3339 time")))
            })
            .transpose()?;
        let status_param = if c.name == "keys" { "status" } else { "state" };
        Ok(Some(Filter {
            before,
            owners: all(call, "owner"),
            teams: all(call, "team"),
            projects: all(call, "project"),
            roles: all(call, "role"),
            types: one_of(all(call, "type"), &KEY_TYPES, "type")?,
            statuses: one_of(all(call, status_param), statuses, status_param)?,
            q: call.param("q").map(str::to_lowercase).filter(|q| !q.is_empty()),
        }))
    }

    /// Whether a key passes every filter but the type and status facets.
    fn keeps_key(
        &self,
        call: &Call,
        facts: &Facts,
        name: &str,
        spec: &Value,
        meta: Option<&ResourceMeta>,
    ) -> bool {
        let owner = spec.get("owner").and_then(Value::as_str);
        let project = spec
            .get("project")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_TENANT);
        if self
            .before
            .is_some_and(|t| facts.key_last_used(meta).is_some_and(|u| u >= t))
        {
            return false;
        }
        if !self.owners.is_empty() && !owner.is_some_and(|o| self.owners.iter().any(|x| x == o)) {
            return false;
        }
        if !self.projects.is_empty() && !self.projects.iter().any(|p| p == project) {
            return false;
        }
        let team = call
            .snap
            .identity
            .projects
            .get(project)
            .and_then(|p| p.parent.clone());
        let team = team.as_deref().unwrap_or(DEFAULT_TENANT);
        if !self.teams.is_empty() && !self.teams.iter().any(|t| t == team) {
            return false;
        }
        self.q.as_ref().is_none_or(|q| {
            let cred = meta.and_then(|m| m.credential.as_ref());
            contains(Some(name), q)
                || contains(spec.get("description").and_then(Value::as_str), q)
                || contains(owner, q)
                || contains(cred.map(|c| c.kid.as_str()), q)
                || contains(cred.map(|c| c.hint.as_str()), q)
        })
    }

    /// Whether a user passes every filter but the state facet.
    fn keeps_user(&self, call: &Call, facts: &Facts, name: &str, spec: &Value) -> bool {
        if self
            .before
            .is_some_and(|t| facts.user_last_active(call, name).is_some_and(|u| u >= t))
        {
            return false;
        }
        let bindings: Vec<(&str, &str)> = spec
            .get("role_bindings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|b| Some((b.get("role")?.as_str()?, b.get("scope")?.as_str()?)))
            .collect();
        if !self.roles.is_empty() && !bindings.iter().any(|(r, _)| self.roles.iter().any(|x| x == r)) {
            return false;
        }
        if !self.teams.is_empty() {
            let projects = &call.snap.identity.projects;
            let in_team = |team: &str| {
                spec.get("primary_team").and_then(Value::as_str) == Some(team)
                    || bindings.iter().any(|(_, scope)| match scope.split_once('/') {
                        Some(("team", t)) => t == team,
                        Some(("project", p)) => {
                            projects.get(p).and_then(|r| r.parent.as_deref()) == Some(team)
                        }
                        _ => false,
                    })
            };
            if !self.teams.iter().any(|t| in_team(t)) {
                return false;
            }
        }
        self.q.as_ref().is_none_or(|q| {
            contains(Some(name), q)
                || contains(spec.get("display_name").and_then(Value::as_str), q)
                || contains(spec.get("email").and_then(Value::as_str), q)
        })
    }

    /// Keeps in `map` what every filter selects, among what `visible` lets the caller see, and
    /// returns the counts over what every filter but the facets (type and status, or state) selects.
    pub(crate) fn select(
        &self,
        call: &Call,
        facts: &Facts,
        collection: &str,
        map: &mut Map<String, Value>,
        visible: impl Fn(&str, &Value) -> bool,
    ) -> Value {
        let metas = call.snap.config.meta.resources.get(collection);
        let mut facet_a: BTreeMap<&str, u64> = BTreeMap::new();
        let mut facet_b: BTreeMap<&str, u64> = BTreeMap::new();
        let mut flagged = 0u64;
        let (a_names, b_names): (&[&str], &[&str]) = if collection == "keys" {
            (&KEY_TYPES, &KEY_STATUSES)
        } else {
            (&USER_STATES, &[])
        };
        for n in a_names {
            facet_a.insert(n, 0);
        }
        for n in b_names {
            facet_b.insert(n, 0);
        }
        map.retain(|name, spec| {
            if !visible(name, spec) {
                return false;
            }
            let meta = metas.and_then(|m| m.get(name));
            if collection == "keys" {
                if !self.keeps_key(call, facts, name, spec, meta) {
                    return false;
                }
                let (t, s) = (
                    facts.key_type(&call.snap.config.doc, spec, meta),
                    facts.key_status(spec, meta),
                );
                *facet_a.entry(t).or_default() += 1;
                *facet_b.entry(s).or_default() += 1;
                (self.types.is_empty() || self.types.iter().any(|x| x == t))
                    && (self.statuses.is_empty() || self.statuses.iter().any(|x| x == s))
            } else {
                if !self.keeps_user(call, facts, name, spec) {
                    return false;
                }
                let s = facts.user_state(name, spec);
                *facet_a.entry(s).or_default() += 1;
                if facts.flagged(call, name) {
                    flagged += 1;
                }
                self.statuses.is_empty() || self.statuses.iter().any(|x| x == s)
            }
        });
        if collection == "keys" {
            json!({"type": facet_a, "status": facet_b})
        } else {
            json!({"state": facet_a, "flagged": flagged})
        }
    }
}

#[cfg(test)]
mod tests {
    use antirion_runtime::state::{KeyCredential, PreviousCredential};
    use serde_json::json;

    use super::*;

    const DAY: i64 = 86_400_000_000;

    fn meta(created: &str, previous_until: Option<&str>) -> ResourceMeta {
        ResourceMeta {
            etag: String::new(),
            generation: 1,
            created_at: created.into(),
            updated_at: created.into(),
            managed_by: "api".into(),
            uid: ResourceUid::random(),
            credential: Some(KeyCredential {
                kid: "kid".into(),
                verifier: "v".into(),
                hint: "h".into(),
                previous: previous_until.map(|u| PreviousCredential {
                    kid: "old".into(),
                    verifier: "v".into(),
                    valid_until: u.into(),
                }),
            }),
            created_by: None,
            disabled_reason: None,
            lifecycle_at: None,
            credentials_rotated_at: std::collections::BTreeMap::new(),
            rotated_at: None,
        }
    }

    #[test]
    fn key_status_takes_the_first_state_that_holds() {
        let facts = Facts {
            now: UtcMicros(10 * DAY),
            ..Facts::default()
        };
        let m = meta("1970-01-01T00:00:00Z", None);
        let at = |d: i64| UtcMicros(d * DAY).to_rfc3339_seconds();
        assert_eq!(facts.key_status(&json!({}), Some(&m)), "active");
        assert_eq!(
            facts.key_status(&json!({"enabled": false, "expires_at": at(1)}), Some(&m)),
            "disabled"
        );
        assert_eq!(
            facts.key_status(&json!({"expires_at": at(9)}), Some(&m)),
            "expired"
        );
        assert_eq!(
            facts.key_status(&json!({"expires_at": at(12)}), Some(&m)),
            "expiring"
        );
        let rotating = meta("1970-01-01T00:00:00Z", Some(&at(11)));
        assert_eq!(
            facts.key_status(&json!({"expires_at": at(12)}), Some(&rotating)),
            "rotating"
        );
        let over = meta("1970-01-01T00:00:00Z", Some(&at(9)));
        assert_eq!(facts.key_status(&json!({}), Some(&over)), "active");
    }

    #[test]
    fn ownerless_keys_are_service_keys() {
        let facts = Facts::default();
        let doc = json!({"users": {"ann": {}, "ci": {"kind": "service"}}});
        let m = meta("1970-01-01T00:00:00Z", None);
        assert_eq!(facts.key_type(&doc, &json!({}), Some(&m)), "service");
        assert_eq!(facts.key_type(&doc, &json!({"owner": "ci"}), Some(&m)), "service");
        assert_eq!(facts.key_type(&doc, &json!({"owner": "ann"}), Some(&m)), "user");
        let short = json!({"owner": "ann", "expires_at": "1970-01-03T00:00:00Z"});
        assert_eq!(facts.key_type(&doc, &short, Some(&m)), "ephemeral");
        let ownerless_short = json!({"expires_at": "1970-01-03T00:00:00Z"});
        assert_eq!(facts.key_type(&doc, &ownerless_short, Some(&m)), "ephemeral");
    }

    #[test]
    fn user_state_and_filters() {
        let mut facts = Facts::default();
        facts.invited.insert("ann".into());
        assert_eq!(facts.user_state("ann", &json!({"enabled": false})), "invited");
        assert_eq!(facts.user_state("bob", &json!({"enabled": false})), "disabled");
        assert_eq!(facts.user_state("bob", &json!({})), "active");
        assert!(one_of(vec!["active".into()], &USER_STATES, "state").is_ok());
        assert!(one_of(vec!["asleep".into()], &USER_STATES, "state").is_err());
        assert!(contains(Some("CI Bot"), "ci b") && !contains(None, "x"));
    }
}

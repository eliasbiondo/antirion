//! `cache.invalidation_rules`: when each rule fires and what its action selects. A firing becomes
//! an invalidation (`crate::invalidate`) with the cutoff at the firing time, or that time less
//! `older_than`.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use antirion_config::cron::Cron;
use antirion_config::scalar::parse_duration;
use parking_lot::Mutex;
use serde_json::Value;

use crate::invalidate::Target;
use crate::meta::dim;

/// What fires a rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    ModelDisabled,
    DeploymentChanged,
    RouteChanged,
    Schedule(Cron),
    /// A request carried a new value of this metadata key within its cache scope.
    MetadataChanged(String),
    /// Only `:run`, which every rule also allows.
    Manual,
}

/// What a rule drops by.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DropKind {
    Model,
    Route,
    Deployment,
    Key,
    EndUser,
    Tag,
    Metadata,
    OlderThan,
}

impl DropKind {
    fn parse(s: &str) -> Option<DropKind> {
        Some(match s {
            "model" => DropKind::Model,
            "route" => DropKind::Route,
            "deployment" => DropKind::Deployment,
            "key" => DropKind::Key,
            "end_user" => DropKind::EndUser,
            "tag" => DropKind::Tag,
            "metadata" => DropKind::Metadata,
            "older_than" => DropKind::OlderThan,
            _ => return None,
        })
    }

    /// The dimension an entry carries the value under.
    #[must_use]
    pub const fn dimension(self) -> &'static str {
        match self {
            DropKind::Model => "model",
            DropKind::Route => "route",
            DropKind::Deployment => "deployment",
            DropKind::Key => "key",
            DropKind::EndUser => "end_user",
            DropKind::Tag => "tag",
            DropKind::Metadata => "metadata",
            DropKind::OlderThan => "older_than",
        }
    }
}

/// One rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub name: String,
    pub enabled: bool,
    pub event: Event,
    pub drop: DropKind,
    pub value: Option<String>,
    pub older_than: Option<Duration>,
}

/// What a firing knows of its cause.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cause<'a> {
    /// The Model, deployment or route a configuration change was about.
    pub subject: Option<&'a str>,
    /// The metadata key, its previous value and the cache scope digest of a metadata change.
    pub metadata: Option<(&'a str, &'a str, &'a str)>,
}

fn rule(name: &str, v: &Value) -> Option<Rule> {
    let event = match v.pointer("/trigger/event").and_then(Value::as_str)? {
        "model_disabled" => Event::ModelDisabled,
        "deployment_changed" => Event::DeploymentChanged,
        "route_changed" => Event::RouteChanged,
        "schedule" => Event::Schedule(Cron::parse(v.pointer("/trigger/schedule")?.as_str()?).ok()?),
        "metadata_changed" => {
            Event::MetadataChanged(v.pointer("/trigger/metadata_key")?.as_str()?.to_owned())
        }
        _ => Event::Manual,
    };
    Some(Rule {
        name: name.to_owned(),
        enabled: v.get("enabled").and_then(Value::as_bool) != Some(false),
        event,
        drop: DropKind::parse(v.pointer("/action/drop")?.as_str()?)?,
        value: v
            .pointer("/action/value")
            .and_then(Value::as_str)
            .map(str::to_owned),
        older_than: v
            .pointer("/action/older_than")
            .and_then(Value::as_str)
            .and_then(parse_duration),
    })
}

/// The rules of a validated document, by name.
#[must_use]
pub fn compile(doc: &Value) -> Vec<Rule> {
    doc.pointer("/cache/invalidation_rules")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, v)| rule(name, v))
        .collect()
}

/// The request metadata keys entries record: those a rule's trigger or action names.
#[must_use]
pub fn metadata_keys(rules: &[Rule]) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for r in rules {
        if let Event::MetadataChanged(k) = &r.event {
            keys.insert(k.clone());
        }
        if r.drop == DropKind::Metadata
            && let Some(v) = &r.value
        {
            keys.insert(v.split_once('=').map_or(v.as_str(), |(k, _)| k).to_owned());
        }
    }
    keys
}

impl Rule {
    /// What the rule drops when it fires for `cause` at `now_ms`, with the cutoff; `None` when the
    /// cause does not supply what the action needs (a manual run of a rule whose trigger names
    /// the value).
    #[must_use]
    pub fn target(&self, cause: &Cause<'_>, now_ms: i64) -> Option<(Target, i64)> {
        if self.drop == DropKind::OlderThan {
            let d = i64::try_from(self.older_than?.as_millis()).unwrap_or(i64::MAX);
            return Some((Target::OlderThan, now_ms.saturating_sub(d)));
        }
        if self.drop == DropKind::Metadata {
            return match (self.value.as_deref(), cause.metadata) {
                (Some(v), _) if v.contains('=') => Some((Target::dim("metadata", v), now_ms)),
                (_, Some((k, old, digest))) => Some((
                    Target::Dim {
                        dim: dim("metadata", &format!("{k}={old}")),
                        digest: Some(digest.to_owned()),
                    },
                    now_ms,
                )),
                _ => None,
            };
        }
        let value = self.value.as_deref().or(cause.subject)?;
        Some((Target::dim(self.drop.dimension(), value), now_ms))
    }
}

/// The most (rule, scope) pairs whose last metadata value a node remembers; beyond it the node
/// starts over, as after a restart.
const SEEN_MAX: usize = 100_000;

/// The last value of each `metadata_changed` rule's key a node saw per cache scope.
#[derive(Debug, Default)]
pub struct Seen {
    map: Mutex<HashMap<(String, String), String>>,
}

impl Seen {
    /// Records `value` for the rule within scope digest `digest`; the previous value when it
    /// differs.
    pub fn changed(&self, rule: &str, digest: &str, value: &str) -> Option<String> {
        let mut map = self.map.lock();
        if map.len() >= SEEN_MAX {
            map.clear();
        }
        let slot = map.entry((rule.to_owned(), digest.to_owned())).or_default();
        if slot == value {
            return None;
        }
        let old = std::mem::replace(slot, value.to_owned());
        (!old.is_empty()).then_some(old)
    }

    pub fn clear(&self) {
        self.map.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn rules_compile_and_target() {
        let doc = json!({"cache": {"invalidation_rules": {
            "on_disable": {"trigger": {"event": "model_disabled"}, "action": {"drop": "model"}},
            "nightly": {"enabled": false, "trigger": {"event": "schedule", "schedule": "0 3 * * *"},
                        "action": {"drop": "older_than", "older_than": "1h"}},
            "pv": {"trigger": {"event": "metadata_changed", "metadata_key": "pv"},
                   "action": {"drop": "metadata", "value": "pv"}},
            "fixed": {"trigger": {"event": "manual"}, "action": {"drop": "metadata", "value": "team=x"}}
        }}});
        let rules = compile(&doc);
        assert_eq!(rules.len(), 4);
        let by = |n: &str| rules.iter().find(|r| r.name == n).unwrap();
        assert!(!by("nightly").enabled);
        assert_eq!(
            by("on_disable").target(
                &Cause {
                    subject: Some("m"),
                    metadata: None
                },
                9
            ),
            Some((Target::dim("model", "m"), 9))
        );
        assert_eq!(by("on_disable").target(&Cause::default(), 9), None);
        assert_eq!(
            by("nightly").target(&Cause::default(), 7_200_000),
            Some((Target::OlderThan, 3_600_000))
        );
        let changed = Cause {
            subject: None,
            metadata: Some(("pv", "1", "abc")),
        };
        assert_eq!(
            by("pv").target(&changed, 5),
            Some((
                Target::Dim {
                    dim: "metadata/pv=1".into(),
                    digest: Some("abc".into())
                },
                5
            ))
        );
        assert_eq!(
            metadata_keys(&rules).into_iter().collect::<Vec<_>>(),
            ["pv", "team"]
        );
    }

    #[test]
    fn seen_reports_changes_only() {
        let s = Seen::default();
        assert_eq!(s.changed("r", "d", "1"), None);
        assert_eq!(s.changed("r", "d", "1"), None);
        assert_eq!(s.changed("r", "d", "2").as_deref(), Some("1"));
        assert_eq!(s.changed("r", "other", "3"), None);
    }
}

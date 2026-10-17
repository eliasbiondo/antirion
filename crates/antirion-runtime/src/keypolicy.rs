//! Key lifetime and rotation policy of a project: `max_key_lifetime` and `key_policy` of its
//! organization, team and project. Each `key_policy` field comes from the narrowest level that sets
//! it; the maximum lifetime is the smallest any level sets.

use std::time::Duration;

use antirion_config::model::DEFAULT_TENANT;
use antirion_config::scalar::parse_duration;
use antirion_config::views::{KeyPolicyView, TenantView, view};
use antirion_core::UtcMicros;
use serde_json::Value;

/// Reminder days when no level sets `reminder_days`.
pub const DEFAULT_REMINDER_DAYS: [u32; 4] = [14, 7, 3, 1];

/// The policy that applies to the keys of one project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPolicy {
    pub default_lifetime: Option<Duration>,
    pub rotation_max_age: Option<Duration>,
    /// Days before an expiry or a rotation due date that send a reminder, largest first.
    pub reminder_days: Vec<u32>,
    pub default_grace_period: Option<Duration>,
    /// The smallest `max_key_lifetime` of the levels, and the level setting it
    /// (`organizations/acme`).
    pub max_lifetime: Option<(Duration, String)>,
}

/// A project's levels, narrowest first: `(collection, name, spec)`.
fn levels<'a>(doc: &'a Value, project: &str) -> Vec<(&'static str, String, Option<&'a Value>)> {
    let get = |c: &str, n: &str| doc.get(c).and_then(|m| m.get(n));
    let parent = |c: &str, n: &str, field: &str| {
        get(c, n)
            .and_then(|s| s.get(field))
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_TENANT)
            .to_owned()
    };
    let team = parent("projects", project, "team");
    let org = parent("teams", &team, "organization");
    vec![
        ("projects", project.to_owned(), get("projects", project)),
        ("teams", team.clone(), get("teams", &team)),
        ("organizations", org.clone(), get("organizations", &org)),
    ]
}

fn duration(v: Option<&String>) -> Option<Duration> {
    v.and_then(|s| parse_duration(s))
}

impl KeyPolicy {
    /// The policy of `project` in `doc`.
    #[must_use]
    pub fn of(doc: &Value, project: &str) -> KeyPolicy {
        let mut policy = KeyPolicy {
            default_lifetime: None,
            rotation_max_age: None,
            reminder_days: Vec::new(),
            default_grace_period: None,
            max_lifetime: None,
        };
        let mut reminders: Option<Vec<u32>> = None;
        for (collection, name, spec) in levels(doc, project) {
            let t: TenantView = spec.map(view).and_then(Result::ok).unwrap_or_default();
            if let Some(max) = duration(t.max_key_lifetime.as_ref())
                && policy.max_lifetime.as_ref().is_none_or(|(m, _)| max < *m)
            {
                policy.max_lifetime = Some((max, format!("{collection}/{name}")));
            }
            let p: KeyPolicyView = t.key_policy.unwrap_or_default();
            policy.default_lifetime = policy
                .default_lifetime
                .or_else(|| duration(p.default_lifetime.as_ref()));
            policy.rotation_max_age = policy
                .rotation_max_age
                .or_else(|| duration(p.rotation_max_age.as_ref()));
            policy.default_grace_period = policy
                .default_grace_period
                .or_else(|| duration(p.default_grace_period.as_ref()));
            if reminders.is_none() {
                reminders = p.reminder_days;
            }
        }
        let mut days = reminders.unwrap_or_else(|| DEFAULT_REMINDER_DAYS.to_vec());
        days.sort_unstable_by(|a, b| b.cmp(a));
        days.dedup();
        policy.reminder_days = days;
        policy
    }

    /// The expiry a key created at `now` without one gets: `default_lifetime`, never beyond the
    /// maximum lifetime.
    #[must_use]
    pub fn default_expiry(&self, now: UtcMicros) -> Option<UtcMicros> {
        let lifetime = match (self.default_lifetime, &self.max_lifetime) {
            (Some(d), Some((max, _))) => d.min(*max),
            (Some(d), None) => d,
            (None, _) => return None,
        };
        Some(now.saturating_add(lifetime))
    }

    /// When a key whose secret was set at `since` is due for rotation.
    #[must_use]
    pub fn rotation_due_at(&self, since: UtcMicros) -> Option<UtcMicros> {
        self.rotation_max_age.map(|age| since.saturating_add(age))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const DAY: u64 = 86_400;

    #[test]
    fn the_narrowest_level_sets_each_field_and_the_smallest_maximum_wins() {
        let doc = json!({
            "organizations": {"acme": {"max_key_lifetime": "30d",
                "key_policy": {"default_lifetime": "90d", "rotation_max_age": "180d", "reminder_days": [1, 7]}}},
            "teams": {"t": {"organization": "acme", "max_key_lifetime": "60d"}},
            "projects": {"p": {"team": "t", "key_policy": {"rotation_max_age": "30d", "default_grace_period": "1d"}}}
        });
        let p = KeyPolicy::of(&doc, "p");
        assert_eq!(p.default_lifetime, Some(Duration::from_secs(90 * DAY)));
        assert_eq!(p.rotation_max_age, Some(Duration::from_secs(30 * DAY)));
        assert_eq!(p.default_grace_period, Some(Duration::from_secs(DAY)));
        assert_eq!(p.reminder_days, vec![7, 1]);
        assert_eq!(
            p.max_lifetime,
            Some((Duration::from_secs(30 * DAY), "organizations/acme".to_owned()))
        );
        assert_eq!(
            p.default_expiry(UtcMicros(0)),
            Some(UtcMicros(i64::try_from(30 * DAY).unwrap() * 1_000_000))
        );
    }

    #[test]
    fn the_default_project_has_the_default_reminders_and_no_limits() {
        let p = KeyPolicy::of(&json!({}), DEFAULT_TENANT);
        assert_eq!(p.reminder_days, DEFAULT_REMINDER_DAYS.to_vec());
        assert!(p.max_lifetime.is_none() && p.default_expiry(UtcMicros(0)).is_none());
        assert!(p.rotation_due_at(UtcMicros(0)).is_none());
    }
}

//! Firing invalidation rules: on the commit that disables a Model or changes a deployment or a
//! route, on their schedule (every node evaluates it; the firing's `once` key in the store lets one
//! of them fire it), when a request's metadata changes, and by hand. Each firing drops what its
//! action selects, shares the cutoff with the other nodes, and is recorded and audited.

use antirion_cache::invalidate::Target;
use antirion_cache::rules::{Cause, Event, Rule};
use antirion_core::RequestId;
use serde_json::{Value, json};

use crate::{Runtime, Snapshot};

const ACTOR: &str = "cache_invalidation_rules";
/// The longest gap in minutes the schedule catches up on after the clock jumped.
const CATCH_UP_MINUTES: i64 = 1440;

/// A rule whose action takes its value from a trigger cannot run by hand.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the rule's action takes its value from its trigger")]
pub struct NeedsTrigger;

fn changed(previous: &Value, next: &Value, collection: &str) -> Vec<String> {
    let names = |d: &Value| {
        d.get(collection)
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let before = previous.get(collection);
    let after = next.get(collection);
    names(previous)
        .into_iter()
        .filter(|n| before.and_then(|c| c.get(n)) != after.and_then(|c| c.get(n)))
        .collect()
}

fn disabled_models(previous: &Value, next: &Value) -> Vec<String> {
    let off = |d: &Value, m: &str| {
        d.get("models")
            .and_then(|c| c.get(m))
            .and_then(|v| v.get("enabled"))
            == Some(&Value::Bool(false))
    };
    next.get("models")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(name, _)| name)
        .filter(|name| {
            previous
                .get("models")
                .and_then(|c| c.get(name.as_str()))
                .is_some()
        })
        .filter(|name| off(next, name) && !off(previous, name))
        .cloned()
        .collect()
}

impl Runtime {
    /// Fires a rule whose cause is known: drops what `target` selects as of `cutoff_ms`, records the
    /// firing and audits it; how many entries the backend dropped here.
    pub async fn fire_cache_rule(&self, rule: &str, trigger: &str, target: Target, cutoff_ms: i64) -> u64 {
        let id = RequestId::generate().as_str().to_owned();
        self.fire(rule, trigger, &target, cutoff_ms, &id, None)
            .await
            .unwrap_or(0)
    }

    /// `None` when `once` was claimed by another firing.
    async fn fire(
        &self,
        rule: &str,
        trigger: &str,
        target: &Target,
        cutoff_ms: i64,
        id: &str,
        once: Option<&str>,
    ) -> Option<u64> {
        let now = self.now();
        if let Some(once) = once {
            match self
                .store
                .record_rule_firing(id, rule, now.0, trigger, 0, Some(once))
                .await
            {
                Ok(true) => {}
                Ok(false) => return None,
                Err(e) => {
                    tracing::warn!(error = %e, rule, "a scheduled cache rule could not be claimed");
                    return None;
                }
            }
        }
        let dropped = match self.state.cache.invalidate(target, cutoff_ms).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, rule, "a cache rule's entries could not all be dropped");
                0
            }
        };
        self.persist_cache_epochs().await;
        let n = i64::try_from(dropped).unwrap_or(i64::MAX);
        let stored = match once {
            Some(_) => self.store.set_rule_firing_dropped(id, n).await,
            None => self
                .store
                .record_rule_firing(id, rule, now.0, trigger, n, None)
                .await
                .map(|_| ()),
        };
        if let Err(e) = stored {
            tracing::warn!(error = %e, rule, "a cache rule firing could not be recorded");
        }
        self.audit_system(
            ACTOR,
            "cache.rule_fired",
            &format!("cache/invalidation_rules/{rule}"),
            json!({"trigger": trigger, "dropped": dropped}),
        )
        .await;
        Some(dropped)
    }

    /// Runs a rule by hand: `None` when no rule has that name.
    pub async fn run_cache_rule(&self, name: &str) -> Option<Result<u64, NeedsTrigger>> {
        let snap = self.gens.load();
        let rule = snap.cache.rules.iter().find(|r| r.name == name)?;
        let now_ms = self.now().0 / 1000;
        let Some((target, cutoff)) = rule.target(&Cause::default(), now_ms) else {
            return Some(Err(NeedsTrigger));
        };
        Some(Ok(self.fire_cache_rule(name, "manual", target, cutoff).await))
    }

    /// Fires the enabled rules a commit's changes trigger.
    pub(crate) async fn note_cache_changes(&self, previous: &Snapshot, next: &Snapshot) {
        let rules: Vec<&Rule> = next.cache.rules.iter().filter(|r| r.enabled).collect();
        if rules.is_empty() {
            return;
        }
        let (before, after) = (&previous.config.doc, &next.config.doc);
        let causes = [
            ("model_disabled", disabled_models(before, after)),
            ("deployment_changed", changed(before, after, "deployments")),
            ("route_changed", changed(before, after, "routes")),
        ];
        let now_ms = self.now().0 / 1000;
        for (event, subjects) in &causes {
            for rule in rules.iter().filter(|r| {
                matches!(
                    (&r.event, *event),
                    (Event::ModelDisabled, "model_disabled")
                        | (Event::DeploymentChanged, "deployment_changed")
                        | (Event::RouteChanged, "route_changed")
                )
            }) {
                for subject in subjects {
                    let cause = Cause {
                        subject: Some(subject),
                        metadata: None,
                    };
                    if let Some((target, cutoff)) = rule.target(&cause, now_ms) {
                        self.fire_cache_rule(&rule.name, event, target, cutoff).await;
                    }
                }
            }
        }
    }

    /// Fires the scheduled rules whose expression matches a minute since the last evaluation.
    pub(super) async fn run_schedules(&self) {
        let now_ms = self.now().0 / 1000;
        let minute = now_ms.div_euclid(60_000);
        let from = {
            let mut last = self.state.cache_console.schedule_minute.lock();
            let from = last.map_or(minute, |l| l + 1).max(minute - CATCH_UP_MINUTES);
            *last = Some(minute);
            from
        };
        if from > minute {
            return;
        }
        let snap = self.gens.load();
        for rule in snap.cache.rules.iter().filter(|r| r.enabled) {
            let Event::Schedule(cron) = &rule.event else {
                continue;
            };
            let Some(at) = (from..=minute).rev().find(|m| cron.matches(*m)) else {
                continue;
            };
            let Some((target, cutoff)) = rule.target(&Cause::default(), now_ms) else {
                continue;
            };
            let id = RequestId::generate().as_str().to_owned();
            let once = format!("{}@{at}", rule.name);
            self.fire(&rule.name, "schedule", &target, cutoff, &id, Some(&once))
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn commits_name_what_they_changed() {
        let before = json!({"models": {"a": {}, "b": {"enabled": false}, "c": {}},
                            "routes": {"r": {"x": 1}, "s": {}}, "deployments": {"d": {}}});
        let after = json!({"models": {"a": {"enabled": false}, "b": {"enabled": false}, "c": {}, "new": {"enabled": false}},
                           "routes": {"r": {"x": 2}, "s": {}}});
        assert_eq!(disabled_models(&before, &after), ["a"]);
        assert_eq!(changed(&before, &after, "routes"), ["r"]);
        assert_eq!(changed(&before, &after, "deployments"), ["d"]);
    }
}

//! Raising, refiring and resolving instances in the store, notifying their channels, and the
//! operator actions on them. Every change is a compare-and-set on the instance's version, so the
//! evaluator, delivery tasks and operators on any node never overwrite each other.

use std::collections::BTreeMap;

use antirion_core::UtcMicros;
use antirion_core::audit::ActorKind;
use antirion_identity::rbac::{AdminPrincipal, Scope};
use antirion_store::{InstanceRow, StoreError};
use serde_json::Value;

use super::config::{Rule, Severity, Target, Trigger};
use super::instance::{Firing, Impact, Instance, Refusal, Stored, fingerprint};
use super::payloads::{self, Phase};
use super::{ChannelJob, Job, uuid};
use crate::Runtime;
use crate::identity::IdentitySnapshot;

/// How many times a conflicting update is retried.
const CAS_TRIES: usize = 8;
/// Event-data members that name the scope an event fired in.
const DIMENSIONS: [&str; 8] = [
    "provider",
    "deployment",
    "model",
    "route",
    "organization",
    "team",
    "project",
    "key",
];

fn parse(row: &InstanceRow) -> Option<Stored> {
    serde_json::from_str(&row.body).ok()
}

/// An instance as read: its summary in today's words.
fn read(rt: &Runtime, row: &InstanceRow) -> Option<Stored> {
    let mut stored = parse(row)?;
    let snap = rt.gens.load();
    super::summaries::reworded(&snap.config.doc, &rt.alerting(), &mut stored.instance);
    Some(stored)
}

/// One instance with its state.
pub async fn load(rt: &Runtime, id: &str) -> Result<Option<Stored>, StoreError> {
    Ok(rt.store.instance(id).await?.and_then(|row| read(rt, &row)))
}

/// Every instance fired at or after `since_us`, newest first.
pub(crate) async fn all(
    rt: &Runtime,
    since_us: Option<i64>,
    fingerprint: Option<&str>,
) -> Result<Vec<Stored>, StoreError> {
    Ok(rt
        .store
        .instances(since_us, fingerprint)
        .await?
        .iter()
        .filter_map(|row| read(rt, row))
        .collect())
}

fn row_of(s: &Stored, version: i64, now: UtcMicros) -> Result<InstanceRow, StoreError> {
    Ok(InstanceRow {
        id: s.instance.id.clone(),
        rule: s.instance.rule.clone(),
        fingerprint: s.instance.fingerprint.clone(),
        status: s.instance.status.clone(),
        fired_us: UtcMicros::parse_rfc3339(&s.instance.fired_at).map_or(now.0, |t| t.0),
        updated_us: now.0,
        version,
        body: serde_json::to_string(s)?,
    })
}

/// Applies `f` to an instance and stores it, retrying when another writer got there first; `None`
/// when there is no such instance.
pub(crate) async fn update<R>(
    rt: &Runtime,
    id: &str,
    mut f: impl FnMut(&mut Stored) -> R,
) -> Result<Option<(Stored, R)>, StoreError> {
    for _ in 0..CAS_TRIES {
        let Some(row) = rt.store.instance(id).await? else {
            return Ok(None);
        };
        let Some(mut stored) = parse(&row) else {
            return Ok(None);
        };
        let out = f(&mut stored);
        if rt
            .store
            .update_instance(row_of(&stored, row.version + 1, rt.now())?)
            .await?
        {
            return Ok(Some((stored, out)));
        }
    }
    Err(StoreError::Conflict)
}

/// Raises `rule` for `scope`: a new instance, or a refire of the open one once `cooldown` has passed
/// since its last fire; nothing within the cooldown of the fingerprint's last fire.
pub(crate) async fn raise(
    rt: &Runtime,
    rule: &Rule,
    scope: BTreeMap<String, String>,
    value: Option<f64>,
    summary: String,
    impact: Impact,
) -> Result<(), StoreError> {
    let now = rt.now();
    let fp = fingerprint(&rule.name, &scope);
    let history = all(rt, None, Some(&fp)).await?;
    if let Some(open) = history.iter().find(|s| s.is_open()) {
        if now.0 - open.state.last_fired_us < rule.cooldown_us {
            if open.state.clear_since_us.is_some() {
                update(rt, &open.instance.id, |s| s.state.clear_since_us = None).await?;
            }
            return Ok(());
        }
        if let Some((s, ())) = update(rt, &open.instance.id, |s| s.refire(now, value, &summary)).await?
            && !s.snoozed(now)
            && maintenance_until(rt, rule, now).is_none()
        {
            notify(rt, &s.instance, Phase::Refired);
        }
        return Ok(());
    }
    if history
        .first()
        .is_some_and(|last| now.0 - last.state.last_fired_us < rule.cooldown_us)
    {
        return Ok(());
    }
    let threshold = match &rule.trigger {
        Trigger::Metric { threshold, .. } => Some(*threshold),
        Trigger::Event { .. } => None,
    };
    let mut stored = Firing {
        id: uuid(),
        rule,
        scope,
        value,
        threshold,
        summary,
        impact,
    }
    .into_stored(now);
    if let Some(until) = maintenance_until(rt, rule, now) {
        stored
            .snooze(now, until, MAINTENANCE)
            .map_err(|_| StoreError::Conflict)?;
    }
    if rt.store.insert_instance(row_of(&stored, 1, now)?).await? && !stored.snoozed(now) {
        notify(rt, &stored.instance, Phase::Fired);
    }
    Ok(())
}

/// Who snoozes the instances a maintenance window silences.
const MAINTENANCE: &str = "maintenance";

/// The end of the maintenance window covering `now`, when `rule` stays silent in one.
fn maintenance_until(rt: &Runtime, rule: &Rule, now: UtcMicros) -> Option<UtcMicros> {
    if !rule.silence_during_maintenance {
        return None;
    }
    crate::operations::maintenance::current(&rt.gens.load().config.doc, now).map(|w| w.end)
}

/// Marks the open instance of a fingerprint clear since `now`, and resolves it once it has been
/// clear for `resolve_after`.
pub(crate) async fn clear(rt: &Runtime, rule: &Rule, open: &Stored) -> Result<(), StoreError> {
    let now = rt.now();
    let since = open.state.clear_since_us.unwrap_or(now.0);
    if now.0 - since >= rule.resolve_after_us {
        resolve(rt, &open.instance.id, None).await?;
    } else if open.state.clear_since_us.is_none() {
        update(rt, &open.instance.id, |s| s.state.clear_since_us = Some(now.0)).await?;
    }
    Ok(())
}

async fn resolve(rt: &Runtime, id: &str, actor: Option<&str>) -> Result<Option<Stored>, StoreError> {
    let now = rt.now();
    match update(rt, id, |s| s.resolve(now, actor)).await? {
        Some((s, Ok(()))) => {
            if !s.snoozed(now) {
                notify(rt, &s.instance, Phase::Resolved);
            }
            Ok(Some(s))
        }
        _ => Ok(None),
    }
}

/// Queues an instance's notifications: each channel of its rule (its escalation channels for an
/// escalation) whose minimum severity it meets, the `alert.*` event for the subscribed webhooks, and,
/// on a first firing, the users subscribed by their preferences.
pub(crate) fn notify(rt: &Runtime, i: &Instance, phase: Phase) {
    let cfg = rt.alerting();
    let Some(rule) = cfg.rule(&i.rule) else { return };
    let severity = Severity::parse(&i.severity).unwrap_or(Severity::Info);
    let names = if phase == Phase::Escalated {
        &rule.escalation
    } else {
        &rule.channels
    };
    let mut direct = Vec::new();
    for name in names {
        let Some(channel) = cfg.channels.get(name) else {
            continue;
        };
        if !channel.enabled || severity < channel.min_severity {
            continue;
        }
        if phase == Phase::Acknowledged
            && !matches!(channel.target, Target::PagerDuty { .. } | Target::Webhook { .. })
        {
            continue;
        }
        if let Target::Webhook { webhook } = &channel.target {
            direct.push(webhook.clone());
        }
        rt.state.alerting.submit(Job::Channel(ChannelJob {
            channel: channel.clone(),
            instance: Box::new(i.clone()),
            phase,
            event_id: uuid(),
            first_attempt: 1,
            created_us: rt.now().0,
        }));
    }
    let snap = rt.gens.load();
    rt.emit(
        &snap,
        None,
        phase.event_type(),
        &payloads::event_data(i, phase),
        &direct,
    );
    if phase == Phase::Fired {
        rt.state.alerting.submit(Job::Subscribers(Box::new(i.clone())));
    }
}

/// The scope an event fired in: the dimensions its data names.
fn event_scope(data: &Value) -> BTreeMap<String, String> {
    let mut scope: BTreeMap<String, String> = DIMENSIONS
        .iter()
        .filter_map(|d| Some(((*d).to_owned(), data.get(*d)?.as_str()?.to_owned())))
        .collect();
    if let Some(b) = data.get("budget")
        && let (Some(level), Some(owner), Some(name)) = (
            b.get("level").and_then(Value::as_str),
            b.get("owner").and_then(Value::as_str),
            b.get("name").and_then(Value::as_str),
        )
    {
        scope.insert("budget".to_owned(), format!("{level}/{owner}/{name}"));
    }
    scope
}

fn event_summary(kind: &str, data: &Value) -> String {
    match kind {
        "deployment.state_changed" => format!(
            "Deployment {} went from {} to {}",
            data.get("deployment").and_then(Value::as_str).unwrap_or("?"),
            data.get("from").and_then(Value::as_str).unwrap_or("?"),
            data.get("to").and_then(Value::as_str).unwrap_or("?"),
        ),
        _ => format!("Event {kind}"),
    }
}

/// Fires the event rules subscribed to `kind`.
pub(crate) async fn on_event(rt: &Runtime, kind: &str, data: &Value) {
    let cfg = rt.alerting();
    for rule in cfg.rules.iter().filter(|r| r.enabled) {
        let Trigger::Event { events } = &rule.trigger else {
            continue;
        };
        if !events.iter().any(|p| crate::telemetry::alerts::matches(p, kind)) {
            continue;
        }
        let scope = event_scope(data);
        if !rule.scope.iter().all(|(k, v)| scope.get(k) == Some(v)) {
            continue;
        }
        if let Err(e) = raise(
            rt,
            rule,
            scope,
            None,
            event_summary(kind, data),
            Impact::default(),
        )
        .await
        {
            tracing::warn!(error = %e, rule = %rule.name, "an event rule could not raise its alert");
        }
    }
}

/// The scopes containing an instance's scope, broadest first.
#[must_use]
pub fn chain(identity: &IdentitySnapshot, scope: &BTreeMap<String, String>) -> Vec<Scope> {
    if let Some(p) = scope.get("project") {
        identity.project_scopes(p)
    } else if let Some(t) = scope.get("team") {
        identity.team_scopes(t)
    } else if let Some(o) = scope.get("organization") {
        vec![Scope::Gateway, Scope::Organization(o.clone())]
    } else {
        vec![Scope::Gateway]
    }
}

fn wanted(prefs: &Value, i: &Instance) -> bool {
    let Some(email) = prefs.pointer("/notifications/email") else {
        return false;
    };
    let listed = |member: &str, value: &str| {
        email
            .get(member)
            .and_then(Value::as_array)
            .is_some_and(|l| l.iter().any(|v| v.as_str() == Some(value)))
    };
    let categories_ok = email
        .get("categories")
        .and_then(Value::as_array)
        .is_none_or(|l| l.is_empty() || listed("categories", &i.category));
    listed("severities", &i.severity) && categories_ok
}

/// Emails a fired instance to every enabled human user who may read it and whose preferences ask
/// for its severity and category, and pushes it to their browsers when they asked for that.
pub(crate) async fn to_subscribers(rt: &Runtime, i: &Instance) {
    let snap = rt.gens.load();
    let w = payloads::Wording::of(&snap.config.doc, &snap.alerting, i);
    let scopes = chain(&snap.identity, &i.scope);
    let link = rt.console_link(&i.id);
    let mut users: Vec<(&String, &crate::identity::UserRt)> = snap
        .identity
        .users
        .iter()
        .filter(|(_, u)| u.enabled && u.human)
        .collect();
    users.sort_by(|a, b| a.0.cmp(b.0));
    for (name, u) in users {
        let principal = AdminPrincipal {
            kind: ActorKind::Operator,
            id: name.clone(),
            user: Some(name.clone()),
            bindings: u.bindings.clone(),
            read_only: false,
        };
        if !principal.allows("alerts:read", &scopes) {
            continue;
        }
        let Ok(prefs) = rt.preferences(name).await else {
            continue;
        };
        let (subject, body) = payloads::email(i, Phase::Fired, link.as_deref(), w);
        if crate::push::wanted(&prefs, &i.severity, &i.category) {
            let message = crate::push::PushMessage {
                title: subject.clone(),
                body: i.summary.clone(),
                url: link.clone(),
                tag: Some(i.id.clone()),
                urgency: crate::push::urgency_of(&i.severity),
            };
            rt.push_to(name, &message).await;
        }
        if wanted(&prefs, i)
            && let Some(to) = &u.email
        {
            rt.send_mail(to, &subject, body);
        }
    }
}

/// An operator action on an instance.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Acknowledge,
    Resolve,
    Snooze(std::time::Duration),
    Unsnooze,
    Note(String),
    Escalate,
}

/// Why an action failed.
#[derive(Debug)]
pub enum ActionError {
    NotFound,
    /// The instance is resolved, or (escalation) its rule has no escalation channel.
    Conflict(&'static str),
    Store(StoreError),
}

impl From<StoreError> for ActionError {
    fn from(e: StoreError) -> ActionError {
        ActionError::Store(e)
    }
}

/// Applies an operator's action and queues what it notifies; returns the instance afterwards.
pub async fn act(rt: &Runtime, id: &str, action: Action, actor: &str) -> Result<Instance, ActionError> {
    let now = rt.now();
    if action == Action::Resolve {
        return match resolve(rt, id, Some(actor)).await? {
            Some(s) => Ok(s.instance),
            None => match load(rt, id).await? {
                Some(_) => Err(ActionError::Conflict("the alert is already resolved")),
                None => Err(ActionError::NotFound),
            },
        };
    }
    let escalation = match &action {
        Action::Escalate => {
            let rule = load(rt, id).await?.ok_or(ActionError::NotFound)?.instance.rule;
            let channels = rt
                .alerting()
                .rule(&rule)
                .map(|r| r.escalation.clone())
                .unwrap_or_default();
            if channels.is_empty() {
                return Err(ActionError::Conflict(
                    "the alert's rule has no escalation channels",
                ));
            }
            channels
        }
        _ => Vec::new(),
    };
    let result = update(rt, id, |s| match &action {
        Action::Acknowledge => s.acknowledge(now, actor),
        Action::Snooze(d) => {
            let until = UtcMicros(now.0.saturating_add(super::config::micros(*d)));
            s.snooze(now, until, actor).map(|()| false)
        }
        Action::Unsnooze => s.unsnooze(now, actor).map(|()| false),
        Action::Note(text) => {
            s.note(now, actor, text);
            Ok(false)
        }
        Action::Escalate => s.escalated(now, actor, &escalation).map(|()| true),
        Action::Resolve => Ok(false),
    })
    .await?;
    let Some((stored, outcome)) = result else {
        return Err(ActionError::NotFound);
    };
    match outcome {
        Err(Refusal::Resolved) => Err(ActionError::Conflict("the alert is resolved")),
        Ok(changed) => {
            match action {
                Action::Acknowledge if changed && !stored.snoozed(now) => {
                    notify(rt, &stored.instance, Phase::Acknowledged);
                }
                Action::Escalate => notify(rt, &stored.instance, Phase::Escalated),
                _ => {}
            }
            Ok(stored.instance)
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn instance(severity: &str, category: &str) -> Instance {
        let mut s = serde_json::from_value::<Instance>(json!({
            "id": "a", "rule": "r", "fingerprint": "f", "category": category, "severity": severity,
            "status": "firing", "snoozed_until": null, "fired_at": "t", "acknowledged_at": null,
            "acknowledged_by": null, "resolved_at": null, "resolved_by": null, "summary": "s",
            "scope": {}, "value": null, "threshold": null, "owner_team": null, "runbook_url": null,
            "impact": {"requests": 0, "errors": 0, "throttled": 0}, "timeline": []}))
        .unwrap();
        s.severity = severity.to_owned();
        s
    }

    #[test]
    fn subscriptions_follow_severity_then_category() {
        let prefs = json!({"notifications": {"email": {"severities": ["critical"]}}});
        assert!(wanted(&prefs, &instance("critical", "latency")));
        assert!(!wanted(&prefs, &instance("warning", "latency")));
        let narrowed =
            json!({"notifications": {"email": {"severities": ["critical"], "categories": ["budget"]}}});
        assert!(!wanted(&narrowed, &instance("critical", "latency")));
        assert!(wanted(&narrowed, &instance("critical", "budget")));
        assert!(!wanted(&json!({}), &instance("critical", "latency")));
    }

    #[test]
    fn event_scopes_take_the_named_dimensions() {
        let s = event_scope(
            &json!({"provider": "p", "deployment": "d", "from": "closed", "to": "open",
                                    "budget": {"level": "key", "owner": "k", "name": "b"}}),
        );
        assert_eq!(s.get("deployment").map(String::as_str), Some("d"));
        assert_eq!(s.get("budget").map(String::as_str), Some("key/k/b"));
        assert!(!s.contains_key("from"));
        assert_eq!(
            event_summary(
                "deployment.state_changed",
                &json!({"deployment": "d", "from": "closed", "to": "open"})
            ),
            "Deployment d went from closed to open"
        );
    }
}

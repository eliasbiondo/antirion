//! Alerting: rules evaluated on a schedule, instances with their lifecycle, and delivery to event
//! webhooks and alert channels; also the audit sinks, service-level objectives and OTLP metric
//! export, which share the delivery machinery.
//!
//! Nothing here runs on the request path. A request that raises an event only queues it (a bounded
//! channel, `Hub::submit`); one worker per node turns queued work into tracked delivery tasks, each
//! retrying with backoff and holding a place in its target's bounded queue. Metric rules are
//! evaluated every `evaluation_interval` from the request-log records the node already keeps, by one
//! node of a cluster at a time (a store lease), so an alert fires once.

pub mod config;
mod deliver;
mod engine;
mod evaluate;
pub mod instance;
pub mod metrics;
pub mod payloads;
pub mod sinks;
pub mod slo;
pub mod status;
mod summaries;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use antirion_store::StoreError;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use config::{AlertingConfig, Severity};
pub(crate) use deliver::env as deliver_env;
pub use deliver::{TestOutcome, redeliver, test_channel, test_webhook};
pub use engine::{Action, ActionError, act, chain, load};

use crate::Runtime;
use crate::telemetry::alerts::AlertWebhook;
use config::Channel;
use instance::Instance;
use payloads::Phase;

/// Work waiting for the node's alerting worker.
const QUEUE: usize = 4096;
/// Deliveries a channel holds at once.
const CHANNEL_QUEUE: usize = 1000;

/// One unit of queued work.
#[derive(Debug)]
pub(crate) enum Job {
    /// An event for one webhook.
    Webhook(WebhookJob),
    /// An instance's notification for one channel.
    Channel(ChannelJob),
    /// An event the event rules may fire on.
    RuleEvent { kind: String, data: serde_json::Value },
    /// A fired instance for the users subscribed to it.
    Subscribers(Box<Instance>),
}

#[derive(Debug)]
pub(crate) struct WebhookJob {
    pub(crate) hook: AlertWebhook,
    pub(crate) event_id: String,
    pub(crate) kind: String,
    pub(crate) body: Vec<u8>,
    /// The number of the first attempt (later for a redelivery).
    pub(crate) first_attempt: i32,
    pub(crate) created_us: i64,
    /// `(instance id, channel name)` when a webhook channel sends an instance.
    pub(crate) instance: Option<(String, String)>,
}

#[derive(Debug)]
pub(crate) struct ChannelJob {
    pub(crate) channel: Channel,
    pub(crate) instance: Box<Instance>,
    pub(crate) phase: Phase,
    pub(crate) event_id: String,
    pub(crate) first_attempt: i32,
    pub(crate) created_us: i64,
}

/// A delivery target's queue: deliveries in progress and when the oldest started.
#[derive(Debug, Default)]
pub(crate) struct Queue {
    pub(crate) depth: Arc<AtomicUsize>,
    pub(crate) started: Mutex<Vec<i64>>,
}

/// The node's alerting state: the work queue, target queues, the previous signing secrets still
/// accepted, and the conditions metric rules are waiting on.
#[derive(Debug)]
pub struct Hub {
    tx: mpsc::Sender<Job>,
    rx: Mutex<Option<mpsc::Receiver<Job>>>,
    queues: Mutex<HashMap<(String, String), Arc<Queue>>>,
    /// Webhook name → (previous secret, valid until in microseconds).
    previous: Mutex<HashMap<String, (String, i64)>>,
    /// Fingerprint → since when its condition has held.
    pending: Mutex<HashMap<String, i64>>,
    /// User → when its last `auth.login_failed` event was sent.
    login_failures: Mutex<HashMap<String, i64>>,
    dropped: AtomicUsize,
    /// When this node started serving, on the gateway's clock.
    started_us: AtomicI64,
    /// Cancelled (and replaced) by a test-mode reset, which ends the deliveries in progress.
    epoch: Mutex<CancellationToken>,
}

impl Default for Hub {
    fn default() -> Hub {
        let (tx, rx) = mpsc::channel(QUEUE);
        Hub {
            tx,
            rx: Mutex::new(Some(rx)),
            queues: Mutex::new(HashMap::new()),
            previous: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            login_failures: Mutex::new(HashMap::new()),
            dropped: AtomicUsize::new(0),
            started_us: AtomicI64::new(i64::MIN),
            epoch: Mutex::new(CancellationToken::new()),
        }
    }
}

impl Hub {
    /// Queues work without waiting; work that finds the queue full is dropped and counted.
    pub(crate) fn submit(&self, job: Job) -> bool {
        if self.tx.try_send(job).is_ok() {
            true
        } else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            tracing::warn!("the alerting queue is full; an alerting task was dropped");
            false
        }
    }

    pub(crate) fn queue(&self, kind: &str, target: &str) -> Arc<Queue> {
        Arc::clone(
            self.queues
                .lock()
                .entry((kind.to_owned(), target.to_owned()))
                .or_default(),
        )
    }

    /// Every target queue of this node: `(kind, target, depth, oldest start)`.
    pub(crate) fn queue_depths(&self) -> Vec<(String, String, usize, Option<i64>)> {
        self.queues
            .lock()
            .iter()
            .map(|((k, t), q)| {
                let oldest = q.started.lock().iter().min().copied();
                (k.clone(), t.clone(), q.depth.load(Ordering::Acquire), oldest)
            })
            .collect()
    }

    /// The previous signing secret of a webhook while it is still accepted at `now_us`.
    pub(crate) fn previous_secret(&self, webhook: &str, now_us: i64) -> Option<String> {
        self.previous
            .lock()
            .get(webhook)
            .filter(|(_, until)| now_us < *until)
            .map(|(s, _)| s.clone())
    }

    pub(crate) fn set_previous_secret(&self, webhook: &str, secret: Option<(String, i64)>) {
        let mut p = self.previous.lock();
        match secret {
            Some(s) => {
                p.insert(webhook.to_owned(), s);
            }
            None => {
                p.remove(webhook);
            }
        }
    }

    /// The share of `[from, to)` this node has been serving.
    pub(crate) fn uptime_ratio(&self, from: i64, to: i64) -> Option<f64> {
        let started = self.started_us.load(Ordering::Relaxed);
        if to <= from || started == i64::MIN {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let ratio = (to - started.clamp(from, to)) as f64 / (to - from) as f64;
        Some(ratio)
    }

    /// The token deliveries started now stop on: a test-mode reset cancels it.
    pub(crate) fn epoch(&self) -> CancellationToken {
        self.epoch.lock().clone()
    }

    /// Forgets queued conditions and ends the deliveries in progress (test-mode reset).
    pub fn reset(&self) {
        self.pending.lock().clear();
        let old = std::mem::replace(&mut *self.epoch.lock(), CancellationToken::new());
        old.cancel();
        self.previous.lock().clear();
    }
}

/// A place in a target's queue, released when the delivery ends.
pub(crate) struct Slot {
    queue: Arc<Queue>,
    started: i64,
}

impl Slot {
    pub(crate) fn take(queue: Arc<Queue>, capacity: usize, now_us: i64) -> Option<Slot> {
        queue
            .depth
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < capacity).then_some(n + 1)
            })
            .ok()?;
        queue.started.lock().push(now_us);
        Some(Slot {
            queue,
            started: now_us,
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut started = self.queue.started.lock();
        if let Some(i) = started.iter().position(|t| *t == self.started) {
            started.swap_remove(i);
        }
        drop(started);
        self.queue.depth.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Starts the node's alerting tasks: the worker, the evaluator, the audit sinks, the queue reports
/// and the OTLP metric exporter. Each stops with the node.
pub fn spawn(rt: &Arc<Runtime>) {
    let Some(rx) = rt.state.alerting.rx.lock().take() else {
        return;
    };
    rt.state.alerting.started_us.store(rt.now().0, Ordering::Relaxed);
    rt.tasks.spawn(worker(Arc::clone(rt), rx));
    rt.tasks.spawn(evaluate::run(Arc::clone(rt)));
    rt.tasks.spawn(sinks::run(Arc::clone(rt)));
    rt.tasks.spawn(status::report(Arc::clone(rt)));
    rt.tasks.spawn(crate::otlp_metrics::run(Arc::clone(rt)));
}

async fn worker(rt: Arc<Runtime>, mut rx: mpsc::Receiver<Job>) {
    loop {
        let job = tokio::select! {
            () = rt.shutdown.cancelled() => return,
            j = rx.recv() => match j {
                Some(j) => j,
                None => return,
            },
        };
        match job {
            Job::Webhook(w) => deliver::start_webhook(&rt, w),
            Job::Channel(c) => deliver::start_channel(&rt, c),
            Job::RuleEvent { kind, data } => engine::on_event(&rt, &kind, &data).await,
            Job::Subscribers(i) => engine::to_subscribers(&rt, &i).await,
        }
    }
}

impl Runtime {
    /// Queues an event for the webhooks of `snap` subscribed to it (only `only` when set, never those
    /// in `except`) and for the event rules that fire on it.
    pub(crate) fn emit(
        &self,
        snap: &crate::Snapshot,
        only: Option<&str>,
        kind: &str,
        data: &serde_json::Value,
        except: &[String],
    ) {
        let now = self.now();
        let hooks: Vec<&AlertWebhook> = crate::telemetry::alerts::targets(&snap.alerts, kind, only)
            .into_iter()
            .filter(|h| !except.contains(&h.name))
            .collect();
        if !hooks.is_empty() {
            let id = uuid();
            let body =
                crate::telemetry::alerts::event(&id, kind, &now.to_rfc3339(), &self.node.id, data.clone())
                    .to_string()
                    .into_bytes();
            for hook in hooks {
                self.state.alerting.submit(Job::Webhook(WebhookJob {
                    hook: hook.clone(),
                    event_id: id.clone(),
                    kind: kind.to_owned(),
                    body: body.clone(),
                    first_attempt: 1,
                    created_us: now.0,
                    instance: None,
                }));
            }
        }
        let ruled = !kind.starts_with("alert.")
            && kind != "test.ping"
            && snap.alerting.rules.iter().any(|r| {
                r.enabled
                    && matches!(&r.trigger, config::Trigger::Event { events }
                        if events.iter().any(|p| crate::telemetry::alerts::matches(p, kind)))
            });
        if ruled {
            self.state.alerting.submit(Job::RuleEvent {
                kind: kind.to_owned(),
                data: data.clone(),
            });
        }
    }

    /// `auth.login_failed` for a user's failed sign-in, at most once a minute per user.
    pub fn login_failed(&self, user: &str, action: &str) {
        let now = self.now().0;
        {
            let mut last = self.state.alerting.login_failures.lock();
            if last.get(user).is_some_and(|t| now >= *t && now - *t < 60_000_000) {
                return;
            }
            last.insert(user.to_owned(), now);
        }
        let snap = self.gens.load();
        self.emit(
            &snap,
            None,
            "auth.login_failed",
            &serde_json::json!({"user": user, "action": action}),
            &[],
        );
    }

    /// `audit.privileged` for a successful change of role bindings, security settings or secrets.
    pub fn audit_privileged(&self, action: &str, resource: &str, actor: Option<&str>) {
        let privileged = resource.starts_with("role_bindings")
            || matches!(
                resource,
                "settings/security" | "settings/auth" | "settings/governance"
            )
            || action.contains("rotate");
        if !privileged {
            return;
        }
        let snap = self.gens.load();
        self.emit(
            &snap,
            None,
            "audit.privileged",
            &serde_json::json!({"action": action, "resource": resource, "actor": actor}),
            &[],
        );
    }

    /// The alerting settings of the serving generation.
    #[must_use]
    pub fn alerting(&self) -> Arc<AlertingConfig> {
        Arc::clone(&self.gens.load().alerting)
    }

    /// A console link to an alert instance, when the console's origin is configured.
    #[must_use]
    pub(crate) fn console_link(&self, instance: &str) -> Option<String> {
        let cfg = self.alerting();
        let origin = cfg.console_origin.as_ref()?;
        Some(format!("{origin}{}/alerts/{instance}", self.statics.ui.path))
    }
}

/// Every instance fired at or after `since_us` (all when `None`), newest first.
pub async fn instances(rt: &Runtime, since_us: Option<i64>) -> Result<Vec<instance::Stored>, StoreError> {
    engine::all(rt, since_us, None).await
}

/// The instances of a fingerprint fired at or after `since_us`, newest first.
pub async fn similar(
    rt: &Runtime,
    fingerprint: &str,
    since_us: i64,
) -> Result<Vec<instance::Stored>, StoreError> {
    engine::all(rt, Some(since_us), Some(fingerprint)).await
}

/// A new webhook signing secret: 32 random bytes, URL-safe base64, prefixed `whsec_`.
pub fn generate_secret() -> Result<String, getrandom::Error> {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)?;
    Ok(format!(
        "whsec_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    ))
}

/// Keeps a webhook's previous signing secret, sealed, accepted until `until_us`; `None` ends any
/// overlap.
pub async fn keep_previous_secret(
    rt: &Runtime,
    webhook: &str,
    previous: Option<(String, i64)>,
) -> Result<(), StoreError> {
    let key = status::previous_key(webhook);
    let stored = match &previous {
        Some((secret, until_us)) => status::Previous {
            sealed: rt
                .sealer
                .seal(&key, secret.as_bytes())
                .map_err(|_| StoreError::Config("the secret could not be sealed".to_owned()))?,
            until_us: *until_us,
        },
        None => status::Previous {
            sealed: String::new(),
            until_us: 0,
        },
    };
    rt.store
        .put_value(&key, serde_json::to_vec(&stored).unwrap_or_default())
        .await?;
    rt.state.alerting.set_previous_secret(webhook, previous);
    Ok(())
}

pub(crate) fn uuid() -> String {
    antirion_core::RequestId::generate().as_str().to_owned()
}

/// A rule the gateway itself raises instances of (`key_anomaly`), with no condition of its own: a
/// configured rule of the same name lends it its channels, escalation, owner team and runbook.
fn builtin_rule(rt: &Runtime, name: &str, category: &str, severity: &str) -> config::Rule {
    let cfg = rt.alerting();
    let configured = cfg.rule(name);
    config::Rule {
        name: name.to_owned(),
        enabled: true,
        trigger: config::Trigger::Event { events: Vec::new() },
        category: category.to_owned(),
        severity: Severity::parse(severity).unwrap_or(Severity::Warning),
        scope: std::collections::BTreeMap::new(),
        window_us: 0,
        for_us: 0,
        cooldown_us: 0,
        resolve_after_us: 0,
        channels: configured.map(|r| r.channels.clone()).unwrap_or_default(),
        escalation: configured.map(|r| r.escalation.clone()).unwrap_or_default(),
        owner_team: configured.and_then(|r| r.owner_team.clone()),
        runbook_url: configured.and_then(|r| r.runbook_url.clone()),
        description: configured.and_then(|r| r.description.clone()),
        silence_during_maintenance: configured.is_some_and(|r| r.silence_during_maintenance),
    }
}

/// What a built-in rule's instance counts as its impact: the requests in its scope over the last
/// 15 minutes, since it has no window of its own.
const BUILTIN_IMPACT_US: i64 = 15 * 60 * 1_000_000;

/// Raises an instance of a built-in rule for `scope`, with the impact of the requests in it.
pub(crate) async fn raise_builtin(
    rt: &Runtime,
    name: &str,
    category: &str,
    severity: &str,
    scope: std::collections::BTreeMap<String, String>,
    summary: String,
) {
    let rule = builtin_rule(rt, name, category, severity);
    let i = match metrics::record_scope(&scope) {
        Some(matched) => {
            let now = rt.now().0;
            let records = slo::records_between(rt, now - BUILTIN_IMPACT_US, now + 1).await;
            metrics::impact(
                records
                    .iter()
                    .map(AsRef::as_ref)
                    .filter(|r| metrics::in_scope(r, &matched)),
            )
        }
        None => metrics::Impact::default(),
    };
    let impact = instance::Impact {
        requests: i.requests,
        errors: i.errors,
        throttled: i.throttled,
    };
    if let Err(e) = engine::raise(rt, &rule, scope, None, summary, impact).await {
        tracing::warn!(error = %e, rule = name, "a built-in alert could not be raised");
    }
}

/// Resolves the open instance of a built-in rule for `scope`, as `actor`.
pub(crate) async fn resolve_builtin(
    rt: &Runtime,
    name: &str,
    scope: &std::collections::BTreeMap<String, String>,
    actor: &str,
) {
    let fp = instance::fingerprint(name, scope);
    let Ok(history) = engine::all(rt, None, Some(&fp)).await else {
        return;
    };
    for open in history.iter().filter(|s| s.is_open()) {
        if let Err(e) = engine::act(rt, &open.instance.id, Action::Resolve, actor).await {
            tracing::warn!(error = ?e, rule = name, "a built-in alert could not be resolved");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_bound_a_queue_and_track_the_oldest() {
        let q = Arc::new(Queue::default());
        let a = Slot::take(Arc::clone(&q), 2, 10).unwrap();
        let b = Slot::take(Arc::clone(&q), 2, 20).unwrap();
        assert!(Slot::take(Arc::clone(&q), 2, 30).is_none());
        assert_eq!(q.started.lock().iter().min().copied(), Some(10));
        drop(a);
        assert_eq!(q.started.lock().iter().min().copied(), Some(20));
        assert_eq!(q.depth.load(Ordering::Acquire), 1);
        drop(b);
        assert_eq!(q.depth.load(Ordering::Acquire), 0);
    }

    #[test]
    fn a_full_hub_drops_and_counts() {
        let hub = Hub::default();
        for _ in 0..QUEUE {
            assert!(hub.submit(Job::RuleEvent {
                kind: "x".into(),
                data: serde_json::Value::Null
            }));
        }
        assert!(!hub.submit(Job::RuleEvent {
            kind: "x".into(),
            data: serde_json::Value::Null
        }));
        assert_eq!(hub.dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn previous_secrets_expire() {
        let hub = Hub::default();
        hub.set_previous_secret("w", Some(("old".into(), 100)));
        assert_eq!(hub.previous_secret("w", 99).as_deref(), Some("old"));
        assert_eq!(hub.previous_secret("w", 100), None);
        hub.set_previous_secret("w", None);
        assert_eq!(hub.previous_secret("w", 0), None);
    }
}

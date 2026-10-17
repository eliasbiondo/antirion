//! Delivery to event webhooks and alert channels: each event a tracked task that retries with the
//! sinks' backoff until the target's `max_retry_duration`, holding a place in the target's bounded
//! queue, waiting while a webhook is paused, and recording every attempt in the delivery log (and,
//! for an instance's notification, on its timeline). Also the test actions and redelivery.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::{Clock, SplitMix64};
use antirion_obs::Fam;
use antirion_obs::sink::backoff;
use antirion_store::{DeliveryRow, EventRow};
use serde_json::{Value, json};

use super::config::{Channel, Target};
use super::instance::Instance;
use super::payloads::{self, Phase};
use super::{CHANNEL_QUEUE, ChannelJob, Job, Slot, WebhookJob, engine, uuid};
use crate::Runtime;
use crate::telemetry::Env;
use crate::telemetry::alerts::{AlertWebhook, event, signature};
use crate::telemetry::deliver::{Sent, send};

/// How long a channel's notification is retried.
const CHANNEL_RETRY: Duration = Duration::from_secs(3600);
const CHANNEL_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a paused webhook is checked again.
const PAUSE_POLL: Duration = Duration::from_millis(500);

/// What a test action reports.
#[derive(Clone, Debug, PartialEq)]
pub struct TestOutcome {
    pub ok: bool,
    pub status_code: Option<u16>,
    pub latency_ms: Option<f64>,
    pub error: Option<String>,
}

impl TestOutcome {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({"ok": self.ok, "status_code": self.status_code, "latency_ms": self.latency_ms, "error": self.error})
    }
}

pub(crate) fn env(rt: &Runtime) -> Env {
    let snap = rt.gens.load();
    Env {
        client: rt.egress.clone(),
        policy: Arc::clone(&snap.security.egress),
        tls: snap.security.egress_tls.clone(),
        metrics: rt.metrics.clone(),
        version: rt.node.version.clone(),
        clock: Arc::clone(&rt.clock),
    }
}

/// The webhook's current settings, when it is still configured.
fn current_hook(rt: &Runtime, name: &str) -> Option<AlertWebhook> {
    rt.gens.load().alerts.iter().find(|h| h.name == name).cloned()
}

/// `x-antirion-signature` with the current secret and, during a rotation's overlap, the previous one.
fn signature_header(rt: &Runtime, hook: &AlertWebhook, t: i64, body: &[u8]) -> Option<String> {
    let current = signature(hook.signing_secret.as_deref()?, t, body)?;
    let previous = rt
        .state
        .alerting
        .previous_secret(&hook.name, rt.now().0)
        .and_then(|s| signature(&s, t, body));
    Some(
        match previous.and_then(|p| p.split_once(',').map(|(_, v)| v.to_owned())) {
            Some(v1) => format!("{current},{v1}"),
            None => current,
        },
    )
}

async fn post_event(
    rt: &Runtime,
    hook: &AlertWebhook,
    event_id: &str,
    kind: &str,
    body: &[u8],
) -> (Sent, Duration) {
    let mut headers = hook.headers.clone();
    headers.push(("x-antirion-event-id".into(), event_id.to_owned()));
    headers.push(("x-antirion-event-type".into(), kind.to_owned()));
    let t = antirion_core::SystemClock.now().0 / 1_000_000;
    if let Some(sig) = signature_header(rt, hook, t, body) {
        headers.push(("x-antirion-signature".into(), sig));
    }
    let started = tokio::time::Instant::now();
    let sent = send(
        &env(rt),
        &hook.url,
        &headers,
        "application/json",
        body.to_vec(),
        false,
        hook.timeout,
    )
    .await;
    (sent, started.elapsed())
}

struct Attempt<'a> {
    event_id: &'a str,
    target_kind: &'a str,
    target: &'a str,
    event_type: &'a str,
    attempt: i32,
    sent: Option<Sent>,
    result: &'a str,
}

async fn record(rt: &Runtime, a: Attempt<'_>) {
    let row = DeliveryRow {
        event_id: a.event_id.to_owned(),
        target_kind: a.target_kind.to_owned(),
        target: a.target.to_owned(),
        event_type: a.event_type.to_owned(),
        attempt: a.attempt,
        time_us: rt.now().0,
        status_code: a.sent.and_then(|s| s.status).map(i32::from),
        result: a.result.to_owned(),
        items: 1,
    };
    if let Err(e) = rt.store.record_delivery(row).await {
        tracing::debug!(error = %e, "a delivery attempt could not be recorded");
    }
}

async fn timeline(rt: &Runtime, instance: Option<&(String, String)>, attempt: i32, sent: Sent) {
    let Some((id, channel)) = instance else { return };
    let now = rt.now();
    let detail = json!({"attempt": attempt, "status_code": sent.status, "result": sent.result()});
    let _ = engine::update(rt, id, |s| s.notified(now, channel, sent.ok(), detail.clone())).await;
}

async fn keep_event(rt: &Runtime, row: EventRow) {
    if let Err(e) = rt.store.put_event(row).await {
        tracing::debug!(error = %e, "an alert event could not be kept for redelivery");
    }
}

fn jitter() -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let u = (SplitMix64::from_entropy().next_u64() >> 11) as f64 / (1u64 << 53) as f64;
    u
}

/// Queues an event for one webhook, or drops and counts it when the webhook's queue is full.
pub(crate) fn start_webhook(rt: &Arc<Runtime>, job: WebhookJob) {
    let queue = rt.state.alerting.queue("webhook", &job.hook.name);
    let Some(slot) = Slot::take(queue, job.hook.queue_capacity, rt.now().0) else {
        rt.metrics
            .add(Fam::AlertDeliveries, &[&job.hook.name, "dropped"], 1.0);
        return;
    };
    rt.tasks.spawn(webhook(Arc::clone(rt), job, slot));
}

async fn webhook(rt: Arc<Runtime>, job: WebhookJob, slot: Slot) {
    let epoch = rt.state.alerting.epoch();
    tokio::select! {
        () = epoch.cancelled() => {}
        () = webhook_attempts(&rt, job, slot) => {}
    }
}

async fn webhook_attempts(rt: &Arc<Runtime>, job: WebhookJob, _slot: Slot) {
    let rt = Arc::clone(rt);
    keep_event(
        &rt,
        EventRow {
            event_id: job.event_id.clone(),
            target_kind: "webhook".into(),
            target: job.hook.name.clone(),
            event_type: job.kind.clone(),
            body: String::from_utf8_lossy(&job.body).into_owned(),
            created_us: job.created_us,
        },
    )
    .await;
    let name = job.hook.name.clone();
    let started = tokio::time::Instant::now();
    let mut attempt = job.first_attempt;
    let mut tries: u32 = 0;
    loop {
        let hook = loop {
            match current_hook(&rt, &name) {
                Some(h) if h.enabled => break h,
                Some(h) if started.elapsed() < h.max_retry_duration => {
                    tokio::select! {
                        () = rt.shutdown.cancelled() => return,
                        () = tokio::time::sleep(PAUSE_POLL) => {}
                    }
                }
                _ => {
                    rt.metrics.add(Fam::AlertDeliveries, &[&name, "dropped"], 1.0);
                    return;
                }
            }
        };
        let (sent, _) = post_event(&rt, &hook, &job.event_id, &job.kind, &job.body).await;
        let base = Attempt {
            event_id: &job.event_id,
            target_kind: "webhook",
            target: &name,
            event_type: &job.kind,
            attempt,
            sent: Some(sent),
            result: sent.result(),
        };
        record(&rt, base).await;
        if let Some((_, channel)) = &job.instance {
            record(
                &rt,
                Attempt {
                    event_id: &job.event_id,
                    target_kind: "channel",
                    target: channel,
                    event_type: &job.kind,
                    attempt,
                    sent: Some(sent),
                    result: sent.result(),
                },
            )
            .await;
        }
        timeline(&rt, job.instance.as_ref(), attempt, sent).await;
        if sent.ok() {
            rt.metrics.add(Fam::AlertDeliveries, &[&name, "delivered"], 1.0);
            return;
        }
        tries = tries.saturating_add(1);
        let wait = backoff(tries, jitter());
        if started.elapsed() + wait > hook.max_retry_duration {
            rt.metrics.add(Fam::AlertDeliveries, &[&name, "dropped"], 1.0);
            tracing::warn!(webhook = %name, "an alert event could not be delivered");
            return;
        }
        rt.metrics.add(Fam::AlertDeliveries, &[&name, "retried"], 1.0);
        attempt = attempt.saturating_add(1);
        tokio::select! {
            () = rt.shutdown.cancelled() => {
                rt.metrics.add(Fam::AlertDeliveries, &[&name, "dropped"], 1.0);
                return;
            }
            () = tokio::time::sleep(wait) => {}
        }
    }
}

/// Queues an instance's notification for one channel.
pub(crate) fn start_channel(rt: &Arc<Runtime>, job: ChannelJob) {
    if let Target::Webhook { webhook } = &job.channel.target {
        let Some(hook) = current_hook(rt, webhook) else {
            return;
        };
        let kind = job.phase.event_type();
        let body = event(
            &job.event_id,
            kind,
            &rt.now().to_rfc3339(),
            &rt.node.id,
            payloads::event_data(&job.instance, job.phase),
        )
        .to_string()
        .into_bytes();
        start_webhook(
            rt,
            WebhookJob {
                hook,
                event_id: job.event_id,
                kind: kind.to_owned(),
                body,
                first_attempt: job.first_attempt,
                created_us: job.created_us,
                instance: Some((job.instance.id.clone(), job.channel.name.clone())),
            },
        );
        return;
    }
    let queue = rt.state.alerting.queue("channel", &job.channel.name);
    let Some(slot) = Slot::take(queue, CHANNEL_QUEUE, rt.now().0) else {
        rt.metrics
            .add(Fam::AlertDeliveries, &[&job.channel.name, "dropped"], 1.0);
        return;
    };
    rt.tasks.spawn(channel(Arc::clone(rt), job, slot));
}

/// One send to a channel (Slack, PagerDuty or email); `None` when the channel has nothing to send
/// for this phase.
async fn send_to_channel(
    rt: &Runtime,
    channel: &Channel,
    i: &Instance,
    phase: Phase,
) -> Option<(Sent, Option<String>)> {
    let link = rt.console_link(&i.id);
    let snap = rt.gens.load();
    let w = payloads::Wording::of(&snap.config.doc, &snap.alerting, i);
    match &channel.target {
        Target::Slack { webhook_url } => {
            let body = payloads::slack(i, phase, link.as_deref(), w)
                .to_string()
                .into_bytes();
            Some((post_json(rt, webhook_url, body).await, None))
        }
        Target::PagerDuty {
            routing_key,
            events_url,
        } => {
            let body = payloads::pagerduty(i, phase, routing_key, link.as_deref())?
                .to_string()
                .into_bytes();
            Some((post_json(rt, events_url, body).await, None))
        }
        Target::Email { to } => {
            let (subject, body) = payloads::email(i, phase, link.as_deref(), w);
            Some(send_mail(rt, to, &subject, body).await)
        }
        Target::Webhook { .. } => None,
    }
}

async fn post_json(rt: &Runtime, url: &str, body: Vec<u8>) -> Sent {
    send(
        &env(rt),
        url,
        &[],
        "application/json",
        body,
        false,
        CHANNEL_TIMEOUT,
    )
    .await
}

/// Sends one message to every address; a failure for any address fails the attempt.
pub(crate) async fn send_mail(
    rt: &Runtime,
    to: &[String],
    subject: &str,
    body: String,
) -> (Sent, Option<String>) {
    let failed = |e: String| {
        (
            Sent {
                status: None,
                timed_out: false,
            },
            Some(e),
        )
    };
    let Some(settings) = rt.gens.load().identity.console.mail.clone() else {
        return failed("outgoing mail is not configured".to_owned());
    };
    for addr in to {
        let out = crate::operators::mail::Outgoing {
            settings: settings.clone(),
            policy: std::sync::Arc::clone(&rt.gens.load().security.egress),
            message: antirion_mail::Message {
                from: settings.from.clone(),
                reply_to: settings.reply_to.clone(),
                to: addr.clone(),
                subject: subject.to_owned(),
                body: body.clone(),
                date: rt.now().to_rfc2822(),
            },
        };
        if let Err(e) = crate::operators::mail::send_now(&out, &rt.metrics).await {
            return failed(e.to_string());
        }
    }
    (
        Sent {
            status: Some(250),
            timed_out: false,
        },
        None,
    )
}

async fn channel(rt: Arc<Runtime>, job: ChannelJob, slot: Slot) {
    let epoch = rt.state.alerting.epoch();
    tokio::select! {
        () = epoch.cancelled() => {}
        () = channel_attempts(&rt, job, slot) => {}
    }
}

async fn channel_attempts(rt: &Arc<Runtime>, job: ChannelJob, _slot: Slot) {
    let rt = Arc::clone(rt);
    let name = job.channel.name.clone();
    let kind = job.phase.event_type();
    keep_event(
        &rt,
        EventRow {
            event_id: job.event_id.clone(),
            target_kind: "channel".into(),
            target: name.clone(),
            event_type: kind.to_owned(),
            body: json!({"phase": phase_name(job.phase), "instance": &*job.instance}).to_string(),
            created_us: job.created_us,
        },
    )
    .await;
    let started = tokio::time::Instant::now();
    let instance = (job.instance.id.clone(), name.clone());
    let mut attempt = job.first_attempt;
    let mut tries: u32 = 0;
    loop {
        let channel = rt
            .alerting()
            .channels
            .get(&name)
            .cloned()
            .unwrap_or_else(|| job.channel.clone());
        let Some((sent, _)) = send_to_channel(&rt, &channel, &job.instance, job.phase).await else {
            return;
        };
        record(
            &rt,
            Attempt {
                event_id: &job.event_id,
                target_kind: "channel",
                target: &name,
                event_type: kind,
                attempt,
                sent: Some(sent),
                result: sent.result(),
            },
        )
        .await;
        if job.phase != Phase::Test {
            timeline(&rt, Some(&instance), attempt, sent).await;
        }
        if sent.ok() {
            rt.metrics.add(Fam::AlertDeliveries, &[&name, "delivered"], 1.0);
            return;
        }
        tries = tries.saturating_add(1);
        let wait = backoff(tries, jitter());
        if started.elapsed() + wait > CHANNEL_RETRY {
            rt.metrics.add(Fam::AlertDeliveries, &[&name, "dropped"], 1.0);
            return;
        }
        rt.metrics.add(Fam::AlertDeliveries, &[&name, "retried"], 1.0);
        attempt = attempt.saturating_add(1);
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
    }
}

const fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Fired => "fired",
        Phase::Refired => "refired",
        Phase::Escalated => "escalated",
        Phase::Acknowledged => "acknowledged",
        Phase::Resolved => "resolved",
        Phase::Test => "test",
    }
}

fn phase_of(s: &str) -> Phase {
    match s {
        "refired" => Phase::Refired,
        "escalated" => Phase::Escalated,
        "acknowledged" => Phase::Acknowledged,
        "resolved" => Phase::Resolved,
        "test" => Phase::Test,
        _ => Phase::Fired,
    }
}

fn outcome(sent: Sent, latency: Duration, error: Option<String>) -> TestOutcome {
    TestOutcome {
        ok: sent.ok(),
        status_code: sent.status,
        latency_ms: Some((latency.as_secs_f64() * 1e6).round() / 1e3),
        error: if sent.ok() {
            None
        } else {
            Some(error.unwrap_or_else(|| match sent.status {
                Some(s) => format!("the endpoint answered {s}"),
                None if sent.timed_out => "the endpoint did not answer in time".to_owned(),
                None => "the endpoint could not be reached".to_owned(),
            }))
        },
    }
}

/// `POST /alerts/webhooks/{name}:test`: one `test.ping` event, sent now; `None` for an unknown webhook.
pub async fn test_webhook(rt: &Runtime, name: &str) -> Option<TestOutcome> {
    let hook = current_hook(rt, name)?;
    let id = uuid();
    let body = event(
        &id,
        "test.ping",
        &rt.now().to_rfc3339(),
        &rt.node.id,
        json!({"test": true, "webhook": name}),
    )
    .to_string()
    .into_bytes();
    let (sent, latency) = post_event(rt, &hook, &id, "test.ping", &body).await;
    record(
        rt,
        Attempt {
            event_id: &id,
            target_kind: "webhook",
            target: name,
            event_type: "test.ping",
            attempt: 1,
            sent: Some(sent),
            result: sent.result(),
        },
    )
    .await;
    Some(outcome(sent, latency, None))
}

/// The instance a channel test sends: marked as a test, never stored.
fn test_instance(rt: &Runtime, channel: &str) -> Instance {
    let now = rt.now();
    Instance {
        id: format!("test-{}", uuid()),
        rule: "test".to_owned(),
        fingerprint: format!("antirion-test-{channel}"),
        category: "test".to_owned(),
        severity: "info".to_owned(),
        status: "firing".to_owned(),
        snoozed_until: None,
        fired_at: now.to_rfc3339(),
        acknowledged_at: None,
        acknowledged_by: None,
        resolved_at: None,
        resolved_by: None,
        summary: "Antirion test notification".to_owned(),
        scope: std::collections::BTreeMap::new(),
        value: None,
        threshold: None,
        owner_team: None,
        runbook_url: None,
        impact: super::instance::Impact::default(),
        timeline: Vec::new(),
    }
}

/// `POST /alerts/channels/{name}:test`: a test notification, sent now; `None` for an unknown
/// channel. A PagerDuty test triggers and then resolves its own incident.
pub async fn test_channel(rt: &Runtime, name: &str) -> Option<TestOutcome> {
    let channel = rt.alerting().channels.get(name).cloned()?;
    if let Target::Webhook { webhook } = &channel.target {
        return Some(test_webhook(rt, webhook).await.unwrap_or(TestOutcome {
            ok: false,
            status_code: None,
            latency_ms: None,
            error: Some("the channel's webhook does not exist".to_owned()),
        }));
    }
    let i = test_instance(rt, name);
    let started = tokio::time::Instant::now();
    let (sent, error) = send_to_channel(rt, &channel, &i, Phase::Test).await?;
    let latency = started.elapsed();
    record(
        rt,
        Attempt {
            event_id: &i.id,
            target_kind: "channel",
            target: name,
            event_type: "test.ping",
            attempt: 1,
            sent: Some(sent),
            result: sent.result(),
        },
    )
    .await;
    if matches!(channel.target, Target::PagerDuty { .. }) && sent.ok() {
        let _ = send_to_channel(rt, &channel, &i, Phase::Resolved).await;
    }
    Some(outcome(sent, latency, error))
}

/// `POST /alerts/deliveries/{event_id}:redeliver`: sends a kept event again to every target it went
/// to, with the same event id; `false` when no such event is kept.
pub async fn redeliver(rt: &Arc<Runtime>, event_id: &str) -> Result<bool, antirion_store::StoreError> {
    let rows = rt.store.event(event_id).await?;
    if rows.is_empty() {
        return Ok(false);
    }
    let (attempts, _) = rt
        .store
        .deliveries(antirion_store::DeliveryQuery {
            limit: 1000,
            ..antirion_store::DeliveryQuery::default()
        })
        .await?;
    for row in rows {
        let next = attempts
            .iter()
            .filter(|(_, d)| d.event_id == event_id && d.target == row.target)
            .map(|(_, d)| d.attempt)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        match row.target_kind.as_str() {
            "webhook" => {
                if let Some(hook) = current_hook(rt, &row.target) {
                    rt.state.alerting.submit(Job::Webhook(WebhookJob {
                        hook,
                        event_id: row.event_id.clone(),
                        kind: row.event_type.clone(),
                        body: row.body.into_bytes(),
                        first_attempt: next,
                        created_us: rt.now().0,
                        instance: None,
                    }));
                }
            }
            _ => {
                let kept: Value = serde_json::from_str(&row.body).unwrap_or(Value::Null);
                let (Some(channel), Ok(instance)) = (
                    rt.alerting().channels.get(&row.target).cloned(),
                    serde_json::from_value::<Instance>(kept["instance"].clone()),
                ) else {
                    continue;
                };
                rt.state.alerting.submit(Job::Channel(ChannelJob {
                    channel,
                    instance: Box::new(instance),
                    phase: phase_of(kept["phase"].as_str().unwrap_or_default()),
                    event_id: row.event_id.clone(),
                    first_attempt: next,
                    created_us: rt.now().0,
                }));
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_round_trip_through_kept_events() {
        for p in [
            Phase::Fired,
            Phase::Refired,
            Phase::Escalated,
            Phase::Acknowledged,
            Phase::Resolved,
            Phase::Test,
        ] {
            assert_eq!(phase_of(phase_name(p)), p);
        }
    }

    #[test]
    fn test_outcomes_explain_failures() {
        let ok = outcome(
            Sent {
                status: Some(200),
                timed_out: false,
            },
            Duration::from_millis(12),
            None,
        );
        assert!(ok.ok && ok.error.is_none() && ok.latency_ms == Some(12.0));
        let bad = outcome(
            Sent {
                status: Some(500),
                timed_out: false,
            },
            Duration::ZERO,
            None,
        );
        assert_eq!(bad.error.as_deref(), Some("the endpoint answered 500"));
        let late = outcome(
            Sent {
                status: None,
                timed_out: true,
            },
            Duration::ZERO,
            None,
        );
        assert!(late.error.unwrap().contains("in time"));
    }
}

//! Settlement: one ledger line per upstream call (or for a cache hit), each priced with the
//! deployment that served it, the counters reconciled to what the request consumed, and the cost the
//! record, the metrics and the cost header report.

use std::sync::Arc;

use antirion_core::UtcMicros;
use antirion_limits::cost::{self, CostComponents, CostStatus};
use antirion_limits::ledger::{BudgetCharge, LedgerLine, LineStatus, Purpose, Reserved, UsageSource};
use antirion_limits::money::{Dec, Nusd};
use antirion_limits::plan::Unit;
use antirion_limits::price::CallTerms;
use antirion_limits::reservation::{BudgetAlert, Reservation, Settlement};
use antirion_limits::settings::OnUnpriced;
use antirion_limits::usage::{Units, Usage};
use antirion_runtime::Snapshot;
use serde_json::{Value, json};

use super::{LineFacts, Pending, Slot};
use crate::reqlog::{AttemptFact, CostFact, Draft, Record, Usage as Reported};

/// How a committed stream ended.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum StreamEnd {
    Completed,
    /// The client went away.
    ClientGone,
    /// The upstream failed after the stream was committed.
    UpstreamFailed,
}

/// How a request ended, as settlement reads it.
#[derive(Clone, Debug)]
pub(crate) enum End {
    /// A complete (non-streaming) response with this status.
    Response(u16),
    /// Replayed from the gateway's cache.
    Cached,
    /// The request failed after admission. A final attempt the upstream answered (a response an
    /// output guardrail then blocked) is still billed at its usage.
    Failed,
    /// The client went away before a response.
    Cancelled,
    /// A committed stream ended, with the usage it reported or that was counted.
    Stream {
        usage: Reported,
        raw: Option<Box<Value>>,
        outcome: StreamEnd,
    },
}

/// One upstream call (or a cache hit) as its ledger line records it.
struct Call<'a> {
    /// The ledger line number: the attempt's position in the record.
    line: usize,
    attempt: Option<&'a AttemptFact>,
    purpose: Purpose,
    status: LineStatus,
    usage: Usage,
    raw: Option<Value>,
    source: UsageSource,
    /// The non-token units the call produced; only the call that served the request has any.
    units: Units,
}

/// The normalised usage of a report, completed by estimates: `input` falls back to `E_in`, a
/// total alone is split with it.
pub(super) fn completed(u: &Reported, e_in: u64) -> (Usage, bool) {
    let mut n = u.normalised();
    let mut estimated = u.estimated;
    match (u.input, u.output, u.total) {
        (None, None, Some(total)) => {
            n.input = e_in.min(total);
            n.output = total.saturating_sub(e_in);
            estimated = true;
        }
        (None, _, _) => {
            n.input = e_in;
            estimated = true;
        }
        _ => {}
    }
    (n, estimated)
}

fn purpose(a: &AttemptFact) -> Purpose {
    match a.reason {
        "fallback" => Purpose::Fallback,
        "retry" => Purpose::Retry,
        "hedge" => Purpose::Hedge,
        _ => Purpose::Primary,
    }
}

/// The usage a failed attempt's error body reported, if any.
fn error_usage(a: &AttemptFact) -> Option<(Usage, Value)> {
    let u = a.usage.filter(|u| !u.is_empty())?;
    Some((u.normalised(), a.usage_raw.clone().unwrap_or(Value::Null)))
}

/// Where the usage of an attempt that did not serve the request comes from: estimated when the
/// gateway completed it (a refused stream that reported none, then asked again), else reported.
fn line_source(usage: Option<&Reported>) -> UsageSource {
    if usage.is_some_and(|u| u.estimated) {
        UsageSource::Estimated
    } else {
        UsageSource::Upstream
    }
}

/// Whether the final attempt is charged as the call that served the request: a complete response,
/// a committed stream, or an upstream answer the gateway then refused (an output guardrail block).
fn served(end: &End, attempt_failed: bool) -> bool {
    match end {
        End::Response(status) => (200..300).contains(status) && !attempt_failed,
        End::Stream { .. } => true,
        End::Failed => !attempt_failed,
        End::Cached | End::Cancelled => false,
    }
}

fn calls<'a>(rec: &'a Record, end: &End, e_in: u64) -> Vec<Call<'a>> {
    if matches!(end, End::Cached) {
        return vec![Call {
            line: 0,
            attempt: None,
            purpose: Purpose::Cache,
            status: LineStatus::CacheHit,
            usage: rec.usage.normalised(),
            raw: rec.usage_raw.clone(),
            source: UsageSource::Cache,
            units: Units::default(),
        }];
    }
    let last = rec.serving_index().unwrap_or(0);
    rec.attempts
        .iter()
        .enumerate()
        .filter(|(_, a)| !a.tail)
        .map(|(i, a)| {
            let served = i == last && served(end, a.failed());
            if served {
                let (reported, raw, status) = match end {
                    End::Stream { usage, raw, outcome } => (
                        *usage,
                        raw.as_deref().cloned(),
                        match outcome {
                            StreamEnd::Completed => LineStatus::Success,
                            StreamEnd::ClientGone => LineStatus::Cancelled,
                            StreamEnd::UpstreamFailed => LineStatus::FailedWithUsage,
                        },
                    ),
                    _ => (rec.usage, rec.usage_raw.clone(), LineStatus::Success),
                };
                let (usage, estimated) = completed(&reported, e_in);
                return Call {
                    line: i,
                    attempt: Some(a),
                    purpose: purpose(a),
                    status,
                    usage,
                    raw,
                    source: if estimated {
                        UsageSource::Estimated
                    } else {
                        UsageSource::Upstream
                    },
                    units: rec.units.clone(),
                };
            }
            let (status, usage, raw) = match error_usage(a) {
                Some((u, raw)) if !a.failed() => (LineStatus::Success, u, Some(raw)),
                Some((u, raw)) => (LineStatus::FailedWithUsage, u, Some(raw)),
                None if matches!(a.class, "client_cancelled" | "hedge_cancelled") => {
                    (LineStatus::Cancelled, Usage::default(), None)
                }
                None => (LineStatus::Failed, Usage::default(), None),
            };
            Call {
                line: i,
                attempt: Some(a),
                purpose: purpose(a),
                status,
                usage,
                raw,
                source: line_source(a.usage.as_ref()),
                units: Units::default(),
            }
        })
        .collect()
}

/// The priced cost of a call: (cost, billed, status, price version, components).
fn price(
    snap: &Snapshot,
    call: &Call<'_>,
    multiplier: Dec,
    service_tier: Option<&str>,
) -> (
    Option<Nusd>,
    Option<Nusd>,
    CostStatus,
    Option<String>,
    CostComponents,
) {
    let free = (
        Some(Nusd::ZERO),
        Some(Nusd::ZERO),
        CostStatus::Exact,
        None,
        CostComponents::default(),
    );
    let Some(a) = call.attempt else {
        return free;
    };
    let base = snap
        .providers
        .deployments
        .get(&a.deployment)
        .map_or(a.upstream_model.as_str(), |d| d.base_model.as_str());
    let Some(p) = snap.pricing.price(&a.provider_type, base, &a.deployment) else {
        let zero = snap.pricing.settings.on_unpriced == OnUnpriced::Zero;
        let n = zero.then_some(Nusd::ZERO);
        return (n, n, CostStatus::Unpriced, None, CostComponents::default());
    };
    let billable = matches!(call.status, LineStatus::Success | LineStatus::Cancelled)
        && (!call.usage.is_zero() || call.units != Units::default());
    let units = Units {
        requests: u64::from(billable),
        ..call.units.clone()
    };
    let terms = CallTerms {
        service_tier,
        batch: false,
    };
    match cost::cost(&p.for_call(terms), &call.usage, &units, None, multiplier) {
        Ok(c) => {
            let status = if call.source == UsageSource::Estimated {
                CostStatus::Estimated
            } else {
                c.status
            };
            (
                Some(c.total),
                Some(c.billed),
                status,
                Some(p.version()),
                c.components,
            )
        }
        Err(_) => (
            None,
            None,
            CostStatus::Estimated,
            Some(p.version()),
            CostComponents::default(),
        ),
    }
}

/// What a cache hit saved: the usage recorded with its entry, billed at the current price of the
/// deployment that produced the entry.
fn avoided_cost(snap: &Snapshot, rec: &Record, multiplier: Dec) -> Option<Nusd> {
    let deployment = rec.cache.origin.as_deref()?;
    let d = snap.providers.deployments.get(deployment)?;
    let p = snap
        .pricing
        .price(d.provider.kind.as_str(), &d.base_model, deployment)?;
    let units = Units {
        requests: 1,
        ..Units::default()
    };
    cost::cost(&p, &rec.usage.normalised(), &units, None, multiplier)
        .ok()
        .map(|c| c.billed)
}

fn charges(r: &Reservation, usage: &Usage, billed: Option<Nusd>) -> Vec<BudgetCharge> {
    r.holds
        .iter()
        .filter_map(|h| {
            let amount = match &h.info.unit {
                Unit::Usd => billed.map_or(0, |b| b.0),
                Unit::BudgetTokens(w) => i128::try_from(w.charge(usage).unwrap_or(0)).unwrap_or(i128::MAX),
                _ => return None,
            };
            (amount != 0).then(|| BudgetCharge {
                level: h.info.level.as_str().to_owned(),
                owner: h.info.owner.clone(),
                name: h.info.name.clone(),
                identity: h.info.identity,
                amount,
            })
        })
        .collect()
}

fn add(a: CostComponents, b: &CostComponents) -> CostComponents {
    let s = |x: Nusd, y: Nusd| Nusd(x.0.saturating_add(y.0));
    CostComponents {
        input: s(a.input, b.input),
        cache_read: s(a.cache_read, b.cache_read),
        cache_write: s(a.cache_write, b.cache_write),
        output: s(a.output, b.output),
        reasoning: s(a.reasoning, b.reasoning),
        audio: s(a.audio, b.audio),
        image: s(a.image, b.image),
        video: s(a.video, b.video),
        unit: s(a.unit, b.unit),
        markup: s(a.markup, b.markup),
    }
}

struct Lines {
    lines: Vec<LedgerLine>,
    settlement: Settlement,
    cost: CostFact,
    /// The request's billed cost, when every call is priced.
    billed: Option<Nusd>,
}

fn lines(p: &Pending, r: &Reservation, rec: &Record, end: &End, now: UtcMicros) -> Lines {
    let f: &LineFacts = &p.facts;
    let calls = calls(rec, end, r.estimate.input);
    let avoided = if matches!(end, End::Cached) {
        avoided_cost(&p.snap, rec, f.multiplier)
    } else {
        None
    };
    let mut total = Usage::default();
    let mut billed_sum: Option<i128> = Some(0);
    let mut components = CostComponents::default();
    let mut statuses: Vec<CostStatus> = Vec::new();
    let mut out = Vec::with_capacity(calls.len());
    for call in &calls {
        let (cost, billed, status, version, comp) =
            price(&p.snap, call, f.multiplier, rec.service_tier.as_deref());
        total = total.plus(&call.usage);
        billed_sum = billed_sum.and_then(|s| billed.map(|b| s.saturating_add(b.0)));
        components = add(components, &comp);
        statuses.push(status);
        let a = call.attempt;
        out.push(LedgerLine {
            request_id: p.request_id.clone(),
            line: u32::try_from(call.line).unwrap_or(u32::MAX),
            purpose: call.purpose,
            shadow: false,
            status: call.status,
            admitted_at: r.admitted_at,
            settled_at: now,
            organization: f.organization.clone(),
            team: f.team.clone(),
            project: f.project.clone(),
            key: f.key.clone(),
            user: f.user.clone(),
            end_user: f.end_user.clone(),
            tags: f.tags.clone(),
            model: Some(f.model.clone()),
            route: f.route.clone(),
            provider: a.map(|a| a.provider.clone()),
            deployment: a.map(|a| a.deployment.clone()),
            upstream_model: a.map(|a| a.upstream_model.clone()),
            call: antirion_limits::ledger::CallTarget::default(),
            endpoint: f.endpoint.to_owned(),
            stream: f.stream,
            usage: call.usage,
            usage_raw: call.raw.clone(),
            usage_source: call.source,
            cost,
            billed,
            cost_status: status,
            price_version: version,
            service_tier: if a.is_some() {
                rec.service_tier.clone()
            } else {
                None
            },
            avoided: avoided.filter(|_| call.status == LineStatus::CacheHit),
            reserved: Reserved {
                input: r.estimate.input,
                output: r.estimate.output,
                usd: r.usd,
                out_source: r.estimate.out_source,
            },
            budgets: charges(r, &call.usage, billed),
        });
    }
    let success = match end {
        End::Response(status) => (200..300).contains(status),
        End::Cached => true,
        End::Stream { outcome, .. } => *outcome == StreamEnd::Completed,
        End::Failed | End::Cancelled => false,
    };
    let billed = billed_sum.map(Nusd);
    let status = [
        CostStatus::Unpriced,
        CostStatus::Estimated,
        CostStatus::Approximate,
    ]
    .into_iter()
    .find(|s| statuses.contains(s))
    .unwrap_or(CostStatus::Exact);
    let charged = matches!(end, End::Cached).then_some(Nusd::ZERO).or(billed);
    Lines {
        lines: out,
        settlement: Settlement {
            usage: if matches!(end, End::Cached) {
                Usage::default()
            } else {
                total
            },
            billed: charged.unwrap_or(Nusd::ZERO),
            success,
        },
        cost: CostFact {
            status: status.as_str(),
            total: charged,
            components,
            saved: avoided,
        },
        billed: charged,
    }
}

fn alert_data(a: &BudgetAlert, request_id: &str) -> Value {
    let amount = |v: u128| match a.unit {
        "usd" => Nusd::from_units(v).to_usd_string(),
        _ => v.to_string(),
    };
    let partition: Vec<String> = a.partition.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let threshold = a.threshold.to_string();
    json!({
        "budget": {
            "level": a.level,
            "owner": a.owner,
            "name": a.name,
            "partition": (!partition.is_empty()).then(|| partition.join(",")),
        },
        "unit": a.unit,
        "amount": amount(a.amount),
        "spent": amount(a.spent),
        "threshold": threshold.parse::<f64>().map_or(Value::String(threshold), Value::from),
        "period_start": a.period.start.to_rfc3339_seconds(),
        "resets_at": a.period.end.map(UtcMicros::to_rfc3339_seconds),
        "request_id": request_id,
    })
}

/// Settles the request once: the counters are reconciled to what it consumed, the ledger lines are
/// appended, and the record gets its cost. A no-op when the request was not admitted or is settled.
pub(crate) async fn settle(slot: &Slot, log: &Draft, end: End) {
    let Some(mut pending) = slot.take() else {
        return;
    };
    let Some(reservation) = pending.reservation.take() else {
        return;
    };
    let rt = Arc::clone(&pending.rt);
    let now = rt.now();
    let Some(built) = log.read(|rec| lines(&pending, &reservation, rec, &end, now)) else {
        rt.accounts.settle(&reservation, &Settlement::default()).await;
        return;
    };
    let report = rt.accounts.settle(&reservation, &built.settlement).await;
    super::record_degraded(log, slot.degrade(report.degraded));
    let sink = pending
        .snap
        .config
        .doc
        .pointer("/budgets/alerts/sink")
        .and_then(Value::as_str);
    for alert in &report.alerts {
        rt.send_alert(
            &pending.snap,
            sink,
            "budget.threshold_crossed",
            alert_data(alert, &pending.request_id),
        );
        rt.budget_threshold_crossed(&pending.snap, alert);
    }
    slot.settled(built.billed, built.cost.components);
    let cost = built.cost;
    log.update(|r| r.cost = Some(cost));
    rt.record_spend(built.lines);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blocked_upstream_answer_is_charged_as_served() {
        assert!(served(&End::Failed, false));
        assert!(!served(&End::Failed, true));
        assert!(served(&End::Response(200), false));
        assert!(!served(&End::Response(400), false));
        assert!(!served(&End::Cancelled, false));
    }

    #[test]
    fn an_estimated_attempt_line_says_so() {
        let estimated = Reported {
            output: Some(4),
            estimated: true,
            ..Reported::default()
        };
        assert_eq!(line_source(Some(&estimated)), UsageSource::Estimated);
        assert_eq!(line_source(Some(&Reported::default())), UsageSource::Upstream);
        assert_eq!(line_source(None), UsageSource::Upstream);
    }

    #[test]
    fn reported_usage_is_completed_by_the_estimate() {
        let reported = Reported {
            input: Some(10),
            output: Some(5),
            ..Reported::default()
        };
        let (u, estimated) = completed(&reported, 99);
        assert_eq!((u.input, u.output, estimated), (10, 5, false));
        let total_only = Reported {
            total: Some(50),
            ..Reported::default()
        };
        let (u, estimated) = completed(&total_only, 12);
        assert_eq!((u.input, u.output, estimated), (12, 38, true));
        let no_input = Reported {
            output: Some(3),
            ..Reported::default()
        };
        let (u, estimated) = completed(&no_input, 7);
        assert_eq!((u.input, u.output, estimated), (7, 3, true));
    }
}

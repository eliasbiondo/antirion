//! The one-line summary of a metric rule's alert: the metric by name, its value and threshold in
//! the metric's own unit (dollars, milliseconds, percentages), and the scope by display names.

use antirion_limits::level::Level;
use serde_json::Value;

use super::config::{AlertingConfig, Rule, Trigger};
use super::instance::Instance;
use crate::wording::{budget_label, collection, display_name, percent, usd, whole};

fn label(metric: &str) -> &str {
    match metric {
        "p95_latency_ms" => "p95 latency",
        "p99_latency_ms" => "p99 latency",
        "ttft_p95_ms" => "p95 time to first token",
        "error_rate" => "Error rate",
        "upstream_5xx_rate" => "Upstream 5xx rate",
        "fallback_rate" => "Fallback rate",
        "rpm_utilization" => "Request-rate utilization",
        "tpm_utilization" => "Token-rate utilization",
        "cost_per_request_usd" => "Cost per request",
        "spend_usd" => "Spend",
        "budget_projection_ratio" => "Projected budget use",
        "guardrail_events" => "Guardrail events",
        "cache_hit_rate" => "Cache hit rate",
        "latency_baseline_ratio" => "Latency against its baseline",
        "cost_baseline_ratio" => "Cost against its baseline",
        "requests" => "Requests",
        other => other,
    }
}

/// A value of `metric` as people read it.
pub(crate) fn shown(metric: &str, v: f64) -> String {
    match metric {
        m if m.ends_with("_usd") => usd(v),
        m if m.ends_with("_ms") => format!("{} ms", whole(v)),
        "latency_baseline_ratio" | "cost_baseline_ratio" => {
            let text = format!("{:.2}", (v * 100.0).round() / 100.0);
            format!("{}×", text.trim_end_matches('0').trim_end_matches('.'))
        }
        m if m.ends_with("_rate") || m.ends_with("_utilization") || m.ends_with("_ratio") => percent(v),
        _ => whole(v),
    }
}

/// `p95 latency 5,012 ms > 4,000 ms for provider Primary`.
pub(crate) fn metric(doc: &Value, rule: &Rule, value: f64) -> String {
    let Trigger::Metric {
        metric,
        operator,
        threshold,
    } = &rule.trigger
    else {
        return rule.name.clone();
    };
    let scope = if rule.scope.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = rule
            .scope
            .iter()
            .map(|(k, v)| format!("{k} {}", display_name(doc, k, v)))
            .collect();
        format!(" for {}", parts.join(", "))
    };
    format!(
        "{} {} {} {}{scope}",
        label(metric),
        shown(metric, value),
        operator.symbol(),
        shown(metric, *threshold)
    )
}

/// The summary of an instance in today's words. Instances raised by older versions kept the raw
/// figures of their time (`Budget team/search/monthly is projected to spend 2067.443506045 of 1200
/// this period`, `error_rate 0.153 > 0.05 (team=search)`); they read like new ones.
pub(crate) fn reworded(doc: &Value, cfg: &AlertingConfig, i: &mut Instance) {
    let fresh = if i.rule == crate::budgets::FORECAST_RULE {
        legacy_forecast(doc, &i.summary, &i.fired_at)
    } else {
        cfg.rule(&i.rule).and_then(|rule| match &rule.trigger {
            Trigger::Metric { metric, .. } if i.summary.starts_with(&format!("{metric} ")) => {
                i.value.map(|v| self::metric(doc, rule, v))
            }
            _ => None,
        })
    };
    if let Some(summary) = fresh {
        i.summary = summary;
    }
}

fn legacy_forecast(doc: &Value, summary: &str, fired_at: &str) -> Option<String> {
    if let Some((label, rest)) = summary.split_once(" is projected to spend ")
        && (label.starts_with("Budget for ") || label.starts_with("Gateway budget ("))
        && let Some((projected, rest)) = rest.split_once(" of ")
        && let Some(amount) = rest.strip_suffix(" this period")
    {
        return Some(crate::wording::forecast_summary(
            label, projected, amount, fired_at,
        ));
    }
    let rest = summary.strip_prefix("Budget ")?;
    let (path, rest) = rest.split_once(" is projected to spend ")?;
    let (spend, rest) = rest.split_once(" of ")?;
    let quota = rest.strip_suffix(" this period")?;
    let mut parts = path.splitn(3, '/');
    let (level, owner, name) = (Level::parse(parts.next()?)?, parts.next()?, parts.next()?);
    let (spend, quota) = (spend.parse::<f64>().ok()?, quota.parse::<f64>().ok()?);
    let declared = match level {
        Level::Gateway => format!("/policies/budgets/{name}"),
        _ => format!("/{}/{owner}/policies/budgets/{name}", collection(level.as_str())?),
    };
    let tokens = doc.pointer(&format!("{declared}/unit")).and_then(Value::as_str) == Some("tokens");
    let amount = |v: f64| {
        if tokens {
            format!("{} tokens", whole(v))
        } else {
            usd(v)
        }
    };
    Some(crate::wording::forecast_summary(
        &budget_label(doc, level, owner, name),
        &amount(spend),
        &amount(quota),
        fired_at,
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::config::AlertingConfig;
    use super::*;

    fn rule(metric: &str, threshold: f64, scope: Value) -> Rule {
        let cfg = AlertingConfig::compile(&json!({"observability": {"alerts": {"rules": {
            "r": {"category": "latency", "metric": metric, "threshold": threshold, "scope": scope}}}}}));
        cfg.rule("r").cloned().unwrap()
    }

    #[test]
    fn summaries_name_metric_value_and_scope() {
        let doc = json!({"teams": {"search": {"display_name": "Search"}}});
        let r = rule("p95_latency_ms", 4000.0, json!({"provider": "p"}));
        assert_eq!(
            metric(&doc, &r, 5012.25),
            "p95 latency 5,012 ms > 4,000 ms for provider p"
        );
        let r = rule("spend_usd", 1200.0, json!({"team": "search"}));
        assert_eq!(
            metric(&doc, &r, 2_067.443_506),
            "Spend $2,067.44 > $1,200.00 for team Search"
        );
        let r = rule("error_rate", 0.05, json!({}));
        assert_eq!(metric(&doc, &r, 0.0725), "Error rate 7.3% > 5%");
        let r = rule("cost_baseline_ratio", 2.0, json!({}));
        assert_eq!(metric(&doc, &r, 2.5), "Cost against its baseline 2.5× > 2×");
    }

    fn instance(rule: &str, summary: &str, value: Option<f64>) -> Instance {
        serde_json::from_value(json!({
            "id": "i", "rule": rule, "fingerprint": "f", "category": "budget", "severity": "warning",
            "status": "firing", "snoozed_until": null, "fired_at": "2026-10-01T00:00:00Z",
            "acknowledged_at": null, "acknowledged_by": null, "resolved_at": null, "resolved_by": null,
            "summary": summary, "scope": {}, "value": value, "threshold": null, "owner_team": null,
            "runbook_url": null, "impact": {"requests": 0, "errors": 0, "throttled": 0}, "timeline": []
        }))
        .unwrap()
    }

    #[test]
    fn summaries_stored_by_older_versions_read_in_todays_words() {
        let doc = json!({"projects": {"zendesk-assistant": {"display_name": "Zendesk assistant"}}});
        let cfg = AlertingConfig::compile(&json!({"observability": {"alerts": {"rules": {
            "r": {"category": "errors", "metric": "error_rate", "threshold": 0.05, "scope": {"team": "search"}}}}}}));
        let mut old = instance(
            crate::budgets::FORECAST_RULE,
            "Budget project/zendesk-assistant/monthly is projected to spend 2067.443506045 of 1200 this period",
            None,
        );
        reworded(&doc, &cfg, &mut old);
        assert_eq!(
            old.summary,
            "Budget for project Zendesk assistant (monthly) was projected on 2026-10-01 to spend $2,067.44 of $1,200.00 \
             this period"
        );
        let mut dated = instance(
            crate::budgets::FORECAST_RULE,
            "Gateway budget (monthly) is projected to spend $5.00 of $3.00 this period",
            None,
        );
        reworded(&doc, &cfg, &mut dated);
        assert_eq!(
            dated.summary,
            "Gateway budget (monthly) was projected on 2026-10-01 to spend $5.00 of $3.00 this period"
        );
        let mut metric_rule = instance("r", "error_rate 0.153 > 0.05 (team=search)", Some(0.153));
        reworded(&doc, &cfg, &mut metric_rule);
        assert_eq!(metric_rule.summary, "Error rate 15.3% > 5% for team search");
        let current = "Error rate 15.3% > 5% for team search";
        let mut new = instance("r", current, Some(0.153));
        reworded(&doc, &cfg, &mut new);
        assert_eq!(new.summary, current);
    }
}

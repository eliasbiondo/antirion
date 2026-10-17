//! Re-costing: ledger lines priced again with the current prices. The difference is appended as an
//! `adjustment` line under the original's request id and admission time; originals are never
//! rewritten. Re-costing lines are numbered from [`RECOST_BASE`], a block of [`PER_LINE`] numbers
//! per original line, so the sum of a line's earlier re-costings is found among the lines of its
//! request and a second run with unchanged prices appends nothing.

use crate::cost::{self, CostOverflow};
use crate::ledger::{LedgerLine, LineStatus};
use crate::money::{Dec, Nusd};
use crate::price::{CallTerms, Price};
use crate::usage::Units;

/// The first line number of re-costing lines.
pub const RECOST_BASE: u32 = 1_000_000;
/// Re-costing line numbers per original line.
pub const PER_LINE: u32 = 1_000;

/// The original line a re-costing line belongs to; `None` for any other line.
#[must_use]
pub fn original_of(line: u32) -> Option<u32> {
    line.checked_sub(RECOST_BASE).map(|l| l / PER_LINE)
}

/// The number of the `k`-th re-costing line of original line `original`.
#[must_use]
pub fn recost_line(original: u32, k: u32) -> Option<u32> {
    (k < PER_LINE).then_some(())?;
    original
        .checked_mul(PER_LINE)?
        .checked_add(k)?
        .checked_add(RECOST_BASE)
}

/// Endpoints whose cost has unit charges a line does not record (images, audio, characters).
const UNIT_PRICED: [&str; 2] = ["images", "audio"];

/// Whether a line can be priced again from what it records: an upstream call of a deployment with
/// token usage, not a cache hit, an adjustment, an expired reservation, a tool or agent call, nor
/// a call to an endpoint with unit charges.
#[must_use]
pub fn repriceable(l: &LedgerLine) -> bool {
    matches!(
        l.status,
        LineStatus::Success | LineStatus::FailedWithUsage | LineStatus::Cancelled
    ) && l.line < RECOST_BASE
        && l.deployment.is_some()
        && l.call.mcp_server.is_none()
        && l.call.mcp_tool.is_none()
        && l.call.a2a_agent.is_none()
        && !UNIT_PRICED.contains(&l.endpoint.as_str())
}

/// A line's cost at a price.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Repriced {
    pub cost: Nusd,
    pub billed: Nusd,
}

/// The cost of `l`'s call at `price` (under the line's service tier, and the batch multiplier for
/// Batches API usage) with `multiplier`.
pub fn reprice(l: &LedgerLine, price: &Price, multiplier: Dec) -> Result<Repriced, CostOverflow> {
    let billable = matches!(l.status, LineStatus::Success | LineStatus::Cancelled) && !l.usage.is_zero();
    let web_searches = l
        .usage_raw
        .as_ref()
        .and_then(|u| u.pointer("/server_tool_use/web_search_requests"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let units = Units {
        requests: u64::from(billable),
        web_searches,
        ..Units::default()
    };
    let terms = CallTerms {
        service_tier: l.service_tier.as_deref(),
        batch: l.endpoint == "batches",
    };
    let c = cost::cost(&price.for_call(terms), &l.usage, &units, None, multiplier)?;
    Ok(Repriced {
        cost: c.total,
        billed: c.billed,
    })
}

#[cfg(test)]
mod tests {
    use antirion_core::UtcMicros;
    use serde_json::json;

    use super::*;
    use crate::cost::CostStatus;
    use crate::estimate::OutSource;
    use crate::ledger::{CallTarget, Purpose, Reserved, UsageSource};
    use crate::usage::Usage;

    fn line(status: LineStatus, endpoint: &str) -> LedgerLine {
        LedgerLine {
            request_id: "r".into(),
            line: 0,
            purpose: Purpose::Primary,
            shadow: false,
            status,
            admitted_at: UtcMicros(0),
            settled_at: UtcMicros(0),
            organization: String::new(),
            team: String::new(),
            project: String::new(),
            key: String::new(),
            user: None,
            end_user: None,
            tags: Vec::new(),
            model: None,
            route: None,
            provider: None,
            deployment: Some("d".into()),
            upstream_model: None,
            call: CallTarget::default(),
            endpoint: endpoint.into(),
            stream: false,
            usage: Usage {
                input: 1_000_000,
                output: 1_000_000,
                ..Usage::default()
            },
            usage_raw: None,
            usage_source: UsageSource::Upstream,
            cost: None,
            billed: None,
            cost_status: CostStatus::Exact,
            price_version: None,
            service_tier: None,
            avoided: None,
            reserved: Reserved {
                input: 0,
                output: 0,
                usd: None,
                out_source: OutSource::Request,
            },
            budgets: Vec::new(),
        }
    }

    #[test]
    fn numbering_is_a_block_per_original_line() {
        assert_eq!(recost_line(0, 0), Some(RECOST_BASE));
        assert_eq!(recost_line(3, 2), Some(RECOST_BASE + 3 * PER_LINE + 2));
        assert_eq!(recost_line(3, PER_LINE), None);
        assert_eq!(original_of(RECOST_BASE + 3 * PER_LINE + 2), Some(3));
        assert_eq!(original_of(1_001), None);
    }

    #[test]
    fn what_can_be_priced_again() {
        assert!(repriceable(&line(LineStatus::Success, "chat_completions")));
        assert!(repriceable(&line(LineStatus::FailedWithUsage, "messages")));
        assert!(!repriceable(&line(LineStatus::CacheHit, "chat_completions")));
        assert!(!repriceable(&line(LineStatus::Adjustment, "chat_completions")));
        assert!(!repriceable(&line(LineStatus::Success, "images")));
        let mut tool = line(LineStatus::Success, "mcp");
        tool.call.mcp_tool = Some("t".into());
        assert!(!repriceable(&tool));
        let mut again = line(LineStatus::Success, "chat_completions");
        again.line = RECOST_BASE;
        assert!(!repriceable(&again));
    }

    #[test]
    fn repricing_uses_the_tier_the_request_and_the_batch_terms() {
        let p = Price::parse(&json!({"input": "1", "output": "2", "per_request": "0.5",
            "service_tiers": {"priority": "2"}, "batch_multiplier": "0.5"}))
        .unwrap();
        let plain = reprice(&line(LineStatus::Success, "chat_completions"), &p, Dec::ONE).unwrap();
        assert_eq!(plain.cost.to_usd_string(), "3.5");
        let mut tiered = line(LineStatus::Success, "chat_completions");
        tiered.service_tier = Some("priority".into());
        assert_eq!(
            reprice(&tiered, &p, Dec::ONE).unwrap().cost.to_usd_string(),
            "6.5"
        );
        let batch = reprice(&line(LineStatus::Success, "batches"), &p, Dec::ONE).unwrap();
        assert_eq!(batch.cost.to_usd_string(), "2");
        let failed = reprice(
            &line(LineStatus::FailedWithUsage, "chat_completions"),
            &p,
            Dec::ONE,
        )
        .unwrap();
        assert_eq!(
            failed.cost.to_usd_string(),
            "3",
            "no per-request charge without a served call"
        );
        let marked = reprice(
            &line(LineStatus::Success, "chat_completions"),
            &p,
            Dec::parse("2").unwrap(),
        )
        .unwrap();
        assert_eq!(marked.billed.to_usd_string(), "7");
    }
}

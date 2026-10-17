//! Limits and budgets of MCP and A2A calls: each `tools/call`, `prompts/get` and `resources/read`,
//! and each A2A RPC, is one request for request limits, with the server, tool and agent as
//! attributes, and a priced tool or agent reserves its per-call price. The reservation is settled
//! when the server or agent answers, and refunded when it never does.

use std::net::IpAddr;
use std::sync::Arc;

use antirion_core::UtcMicros;
use antirion_error::GatewayError;
use antirion_limits::applicable::RequestAttrs;
use antirion_limits::cost::{self, FixedUnits};
use antirion_limits::estimate::{Estimate, OutSource};
use antirion_limits::ledger::{
    BudgetCharge, CallTarget, LedgerLine, LineStatus, Purpose, Reserved, UsageSource,
};
use antirion_limits::money::{Dec, Nusd};
use antirion_limits::plan::{AdmitInput, Unit, plan};
use antirion_limits::price::Price;
use antirion_limits::reservation::{Reservation, Settlement};
use antirion_limits::store::ReservationId;
use antirion_limits::usage::{Units, Usage};
use antirion_limits::weights::Incurred;
use antirion_mcp::{CallCharge, CallLimits, Counted, Pending};
use antirion_runtime::identity::KeyRt;
use antirion_runtime::{Runtime, Snapshot};

/// The caller of MCP and A2A calls, as limits see it.
#[derive(Debug)]
pub(crate) struct McpLimits {
    pub(crate) rt: Arc<Runtime>,
    pub(crate) snap: Arc<Snapshot>,
    pub(crate) key: Arc<KeyRt>,
    pub(crate) end_user: Option<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) ip: Option<IpAddr>,
    /// The request's log id: the reservation's and the ledger line's request id.
    pub(crate) log_id: String,
    /// `mcp` or `a2a`, the endpoint family.
    pub(crate) family: &'static str,
}

/// The per-call price of a call: `pricing.tools` by `"<server>/<tool>"`, then `"<server>/*"`;
/// `pricing.agents` by agent name.
fn per_call(snap: &Snapshot, target: &CallTarget) -> Option<Dec> {
    let pricing = &snap.limits.pricing;
    if let Some(agent) = &target.a2a_agent {
        return pricing.agents.get(agent).copied();
    }
    let server = target.mcp_server.as_deref()?;
    let tool = target.mcp_tool.as_deref()?;
    pricing
        .tools
        .get(&format!("{server}/{tool}"))
        .or_else(|| pricing.tools.get(&format!("{server}/*")))
        .copied()
}

fn target_of(call: Counted<'_>) -> CallTarget {
    match call {
        Counted::Mcp { server, method, item } => CallTarget {
            mcp_server: Some(server.to_owned()),
            mcp_tool: (method == "tools/call").then(|| item.to_owned()),
            a2a_agent: None,
        },
        Counted::Agent { agent, .. } => CallTarget {
            a2a_agent: Some(agent.to_owned()),
            ..CallTarget::default()
        },
    }
}

fn reservation_id(log_id: &str) -> ReservationId {
    let hex: String = log_id.chars().filter(char::is_ascii_hexdigit).collect();
    ReservationId(u128::from_str_radix(&hex, 16).unwrap_or(0))
}

impl CallLimits for McpLimits {
    fn admit<'a>(
        &'a self,
        call: Counted<'a>,
    ) -> Pending<'a, Result<Option<Box<dyn CallCharge>>, GatewayError>> {
        Box::pin(async move {
            let key = &self.key;
            let target = target_of(call);
            let price = per_call(&self.snap, &target);
            let multiplier =
                self.snap
                    .pricing
                    .multiplier(&key.organization, &key.team, &key.project, &key.name);
            let free = Price::default();
            let candidates = [Some(&free)];
            let now = self.rt.now();
            let input = AdmitInput {
                uid: reservation_id(&self.log_id),
                attrs: RequestAttrs {
                    organization: &key.organization,
                    team: &key.team,
                    project: &key.project,
                    key: &key.name,
                    user: key.owner.as_deref(),
                    end_user: self.end_user.as_deref(),
                    endpoint: self.family,
                    tags: &self.tags,
                    ip: self.ip,
                    mcp_server: target.mcp_server.as_deref(),
                    mcp_tool: target.mcp_tool.as_deref(),
                    a2a_agent: target.a2a_agent.as_deref(),
                    priority_class: None,
                    ..RequestAttrs::default()
                },
                estimate: Estimate {
                    input: 0,
                    output: 0,
                    out_source: OutSource::Request,
                    clamp_to: None,
                },
                hedge: 1,
                incurs: Incurred::default(),
                candidates: &candidates,
                fixed: FixedUnits {
                    per_call: price,
                    ..FixedUnits::default()
                },
                multiplier,
                now,
            };
            let planned = plan(&self.snap.limits, &input)?;
            if planned.holds.is_empty() && price.is_none() {
                return Ok(None);
            }
            let reservation = self
                .rt
                .accounts
                .admit(planned)
                .await
                .inspect_err(|e| crate::account::count_refusal(&self.rt.metrics, e))?;
            let charge: Box<dyn CallCharge> = Box::new(Charge {
                rt: Arc::clone(&self.rt),
                reservation: Some(reservation),
                line: price.map(|p| LineTemplate {
                    caller: Caller::of(self),
                    target,
                    price: p,
                    multiplier,
                    admitted_at: now,
                }),
            });
            Ok(Some(charge))
        })
    }
}

/// The tenancy a call's ledger line records.
#[derive(Debug)]
struct Caller {
    organization: String,
    team: String,
    project: String,
    key: String,
    user: Option<String>,
    end_user: Option<String>,
    tags: Vec<String>,
    request_id: String,
    endpoint: &'static str,
}

impl Caller {
    fn of(l: &McpLimits) -> Caller {
        Caller {
            organization: l.key.organization.clone(),
            team: l.key.team.clone(),
            project: l.key.project.clone(),
            key: l.key.name.clone(),
            user: l.key.owner.clone(),
            end_user: l.end_user.clone(),
            tags: l.tags.clone(),
            request_id: l.log_id.clone(),
            endpoint: l.family,
        }
    }
}

/// What a priced call's ledger line needs once it is answered.
#[derive(Debug)]
struct LineTemplate {
    caller: Caller,
    target: CallTarget,
    price: Dec,
    multiplier: Dec,
    admitted_at: UtcMicros,
}

/// An admitted call, until it is answered or dropped.
#[derive(Debug)]
struct Charge {
    rt: Arc<Runtime>,
    reservation: Option<Reservation>,
    line: Option<LineTemplate>,
}

fn line_of(t: &LineTemplate, r: &Reservation, billed: Nusd, total: Nusd, now: UtcMicros) -> LedgerLine {
    let c = &t.caller;
    let budgets = r
        .holds
        .iter()
        .filter(|h| h.info.unit == Unit::Usd && billed.0 != 0)
        .map(|h| BudgetCharge {
            level: h.info.level.as_str().to_owned(),
            owner: h.info.owner.clone(),
            name: h.info.name.clone(),
            identity: h.info.identity,
            amount: billed.0,
        })
        .collect();
    LedgerLine {
        request_id: c.request_id.clone(),
        line: 0,
        purpose: Purpose::Primary,
        shadow: false,
        status: LineStatus::Success,
        admitted_at: t.admitted_at,
        settled_at: now,
        organization: c.organization.clone(),
        team: c.team.clone(),
        project: c.project.clone(),
        key: c.key.clone(),
        user: c.user.clone(),
        end_user: c.end_user.clone(),
        tags: c.tags.clone(),
        model: None,
        route: None,
        provider: None,
        deployment: None,
        upstream_model: None,
        call: t.target.clone(),
        endpoint: c.endpoint.to_owned(),
        stream: false,
        usage: Usage::default(),
        usage_raw: None,
        usage_source: UsageSource::Upstream,
        cost: Some(total),
        billed: Some(billed),
        cost_status: cost::CostStatus::Exact,
        price_version: None,
        service_tier: None,
        avoided: None,
        reserved: Reserved {
            input: 0,
            output: 0,
            usd: r.usd,
            out_source: OutSource::Request,
        },
        budgets,
    }
}

impl CallCharge for Charge {
    fn answered(mut self: Box<Self>, ok: bool) -> Pending<'static, ()> {
        Box::pin(async move {
            let Some(r) = self.reservation.take() else {
                return;
            };
            let priced = self.line.as_ref().and_then(|t| {
                let c = cost::cost(
                    &Price::default(),
                    &Usage::default(),
                    &Units::default(),
                    Some(t.price),
                    t.multiplier,
                )
                .ok()?;
                Some((t, c.billed, c.total))
            });
            let billed = priced.map_or(Nusd::ZERO, |(_, b, _)| b);
            self.rt
                .accounts
                .settle(
                    &r,
                    &Settlement {
                        usage: Usage::default(),
                        billed,
                        success: ok,
                    },
                )
                .await;
            if let Some((t, billed, total)) = priced {
                let line = line_of(t, &r, billed, total, self.rt.now());
                self.rt.record_spend(vec![line]);
            }
        })
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        let Some(r) = self.reservation.take() else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let rt = Arc::clone(&self.rt);
        self.rt.tasks.spawn(async move {
            rt.accounts.settle(&r, &Settlement::default()).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tool_calls_name_a_tool() {
        let t = target_of(Counted::Mcp {
            server: "s",
            method: "tools/call",
            item: "web_search",
        });
        assert_eq!(
            (t.mcp_server.as_deref(), t.mcp_tool.as_deref()),
            (Some("s"), Some("web_search"))
        );
        let p = target_of(Counted::Mcp {
            server: "s",
            method: "prompts/get",
            item: "greet",
        });
        assert_eq!(p.mcp_tool, None);
        let a = target_of(Counted::Agent {
            agent: "planner",
            method: "message/send",
        });
        assert_eq!(a.a2a_agent.as_deref(), Some("planner"));
    }
}

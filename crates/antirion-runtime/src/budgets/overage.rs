//! Overage requests: an `approval` budget that refuses opens one per period for its owner, who
//! approves it (a grant) or denies it; open requests expire at the end of their period. Also the
//! once-per-period handling of a budget's first refusal.

use antirion_core::UtcMicros;
use antirion_limits::applicable::named_budget;
use antirion_limits::overlay::Tally;
use antirion_store::{OverageRow, StoreError, TallyRow};
use serde_json::json;

use super::events::{EventBudget, once_key};
use super::grants::NewGrant;
use crate::{Runtime, Snapshot};

/// What a decider decides about an open request.
#[derive(Clone, Debug)]
pub enum Decision {
    Approve {
        amount: u128,
        expires: UtcMicros,
        reason: Option<String>,
    },
    Deny {
        reason: Option<String>,
    },
}

/// Why a decision was not made.
#[derive(Debug, thiserror::Error)]
pub enum DecideError {
    #[error("the request is no longer open")]
    NotOpen,
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl Runtime {
    /// Writes the counts this node gathered since the last call and handles each budget's first
    /// refusal of a period: the `exhausted` event and `budget.exhausted`, and for an `approval`
    /// budget the period's overage request.
    pub async fn flush_budget_tallies(&self) {
        let tallies = self.accounts.overlay().take();
        if tallies.is_empty() {
            return;
        }
        let rows = tallies
            .iter()
            .map(|t| TallyRow {
                identity: t.budget.identity.hex(),
                period_start_us: t.budget.period.start.0,
                node: self.node.id.clone(),
                refused: i64::try_from(t.refused).unwrap_or(i64::MAX),
                downgraded: i64::try_from(t.downgraded).unwrap_or(i64::MAX),
            })
            .collect();
        if let Err(e) = self.store.add_tallies(rows).await {
            tracing::warn!(error = %e, "budget request counts could not be recorded");
        }
        let snap = self.gens.load();
        for t in tallies.iter().filter(|t| t.refused > 0) {
            self.exhausted(&snap, t).await;
        }
    }

    async fn exhausted(&self, snap: &Snapshot, t: &Tally) {
        let b = &t.budget;
        let Some((_, decl)) = super::declaration(&snap.limits, b.level, &b.owner, &b.name) else {
            return;
        };
        let event = EventBudget {
            level: b.level,
            owner: &b.owner,
            name: &b.name,
            identity: Some(b.identity),
        };
        let resets_at = b.period.end.map(UtcMicros::to_rfc3339_seconds);
        let detail = json!({"period_start": b.period.start.to_rfc3339_seconds(), "resets_at": resets_at});
        let once = once_key("exhausted", b.identity, b.period.start);
        if self
            .record_budget_event("exhausted", event, None, &detail, Some(once))
            .await
        {
            let data = json!({
                "budget": {"level": b.level.as_str(), "owner": b.owner, "name": b.name},
                "period_start": b.period.start.to_rfc3339_seconds(),
                "resets_at": resets_at,
            });
            self.send_alert(snap, None, "budget.exhausted", data);
        }
        if !decl.approval {
            return;
        }
        let project = super::project_of(snap, b.level, &b.owner);
        let Some(named) = named_budget(
            &snap.limits,
            b.level,
            &b.owner,
            &b.name,
            &b.partition,
            project.as_deref(),
        ) else {
            return;
        };
        let spent = self
            .accounts
            .budget_state(&named, self.now())
            .await
            .map_or(0, |s| s.view.spent);
        let request = OverageRow {
            id: format!("ovr_{}", antirion_core::RequestId::generate().as_str()),
            identity: b.identity.hex(),
            period_start_us: b.period.start.0,
            period_end_us: b.period.end.map(|e| e.0),
            level: b.level.as_str().to_owned(),
            owner: b.owner.clone(),
            name: b.name.clone(),
            opened_us: self.now().0,
            spent: spent.to_string(),
            amount: named.quota.to_string(),
            state: "open".to_owned(),
            version: 0,
            decided_by: None,
            decided_us: None,
            reason: None,
            grant: None,
        };
        let id = request.id.clone();
        match self.store.open_overage(request).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                tracing::warn!(error = %e, "an overage request could not be opened");
                return;
            }
        }
        let detail = json!({
            "request": id,
            "spent": super::amount_text(decl, spent),
            "amount": super::amount_text(decl, named.quota),
            "owner": decl.owner,
        });
        self.record_budget_event("overage_requested", event, None, &detail, None)
            .await;
        let label = crate::wording::budget_label(&snap.config.doc, b.level, &b.owner, &b.name);
        let body = format!(
            "{label} reached its amount of {} and refuses requests until the period ends. \
             Approve or deny overage request {id} in the console.\n\nBudget: {}\n",
            crate::wording::budget_amount(&decl.unit, named.quota),
            super::budget_path(b.level, &b.owner, &b.name)
        );
        self.mail_owner(snap, decl, &format!("Overage requested: {label}"), body);
    }

    /// Approves or denies an open request. An approval grants `amount` until `expires` on the
    /// request's budget, granted by `by`.
    pub async fn decide_overage(
        &self,
        mut row: OverageRow,
        d: Decision,
        by: &str,
    ) -> Result<OverageRow, DecideError> {
        let now = self.now();
        if row.state != "open" || row.period_end_us.is_some_and(|e| e <= now.0) {
            return Err(DecideError::NotOpen);
        }
        let (state, reason) = match &d {
            Decision::Approve { reason, .. } => ("approved", reason.clone()),
            Decision::Deny { reason } => ("denied", reason.clone()),
        };
        let grant_id = match &d {
            Decision::Approve { .. } => {
                Some(format!("grant_{}", antirion_core::RequestId::generate().as_str()))
            }
            Decision::Deny { .. } => None,
        };
        let decided = OverageRow {
            state: state.to_owned(),
            decided_by: Some(by.to_owned()),
            decided_us: Some(now.0),
            reason: reason.clone(),
            grant: grant_id,
            ..row.clone()
        };
        if !self.store.decide_overage(&decided).await? {
            return Err(DecideError::NotOpen);
        }
        let grant_id = decided.grant.clone();
        row = OverageRow {
            version: row.version + 1,
            ..decided
        };
        if let Decision::Approve {
            amount,
            expires,
            reason,
        } = d
        {
            let Some(identity) = antirion_limits::counter::CounterId::from_hex(&row.identity) else {
                return Ok(row);
            };
            let Some(level) = antirion_limits::level::Level::parse(&row.level) else {
                return Ok(row);
            };
            let budget = EventBudget {
                level,
                owner: &row.owner,
                name: &row.name,
                identity: Some(identity),
            };
            let grant = NewGrant {
                id: grant_id,
                amount,
                expires,
                reason: reason.unwrap_or_else(|| format!("overage request {}", row.id)),
                granted_by: by.to_owned(),
                request: Some(row.id.clone()),
            };
            self.grant_overage(budget, identity, grant).await?;
        }
        Ok(row)
    }
}

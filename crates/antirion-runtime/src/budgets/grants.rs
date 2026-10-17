//! Overage grants: time-boxed raises of a budget's effective amount, kept in the store and loaded
//! into every node's memory, where admission applies them.

use std::collections::HashMap;

use antirion_core::UtcMicros;
use antirion_limits::counter::CounterId;
use antirion_limits::overlay::ActiveGrant;
use antirion_store::{GrantRow, StoreError};
use serde_json::json;

use super::events::EventBudget;
use crate::Runtime;

/// A grant to make.
#[derive(Clone, Debug)]
pub struct NewGrant {
    /// The id to give it; a new one when absent.
    pub id: Option<String>,
    /// In the budget's units.
    pub amount: u128,
    pub expires: UtcMicros,
    pub reason: String,
    pub granted_by: String,
    /// The overage request it approves.
    pub request: Option<String>,
}

impl Runtime {
    /// Records a grant on a budget, applies it on this node at once, and announces it
    /// (`overage_granted` event, `budget.overage_granted`).
    pub async fn grant_overage(
        &self,
        b: EventBudget<'_>,
        identity: CounterId,
        g: NewGrant,
    ) -> Result<GrantRow, StoreError> {
        let now = self.now();
        let row = GrantRow {
            id: g
                .id
                .unwrap_or_else(|| format!("grant_{}", antirion_core::RequestId::generate().as_str())),
            identity: identity.hex(),
            level: b.level.as_str().to_owned(),
            owner: b.owner.to_owned(),
            name: b.name.to_owned(),
            amount: g.amount.to_string(),
            created_us: now.0,
            expires_us: g.expires.0,
            reason: g.reason,
            granted_by: g.granted_by,
            revoked_us: None,
            request: g.request,
        };
        self.store.insert_grant(row.clone()).await?;
        self.reload_grants().await;
        let snap = self.gens.load();
        let shown = super::declaration(&snap.limits, b.level, b.owner, b.name)
            .map_or_else(|| row.amount.clone(), |(_, d)| super::amount_text(d, g.amount));
        let detail = json!({
            "grant": row.id, "amount": shown, "expires_at": g.expires.to_rfc3339_seconds(),
            "reason": row.reason, "request": row.request,
        });
        let event = EventBudget {
            identity: Some(identity),
            ..b
        };
        self.record_budget_event("overage_granted", event, Some(&row.granted_by), &detail, None)
            .await;
        let data = json!({
            "budget": {"level": b.level.as_str(), "owner": b.owner, "name": b.name},
            "grant": {"id": row.id, "amount": shown, "expires_at": g.expires.to_rfc3339_seconds(),
                      "reason": row.reason, "granted_by": row.granted_by},
        });
        self.send_alert(&snap, None, "budget.overage_granted", data);
        Ok(row)
    }

    /// Revokes a running grant of the budget `identity`; `false` when there is none with that id.
    pub async fn revoke_overage_grant(&self, identity: CounterId, id: &str) -> Result<bool, StoreError> {
        let Some(g) = self.store.grant(id).await? else {
            return Ok(false);
        };
        if g.identity != identity.hex() {
            return Ok(false);
        }
        let revoked = self.store.revoke_grant(id, self.now().0).await?;
        self.reload_grants().await;
        Ok(revoked)
    }

    /// Loads every running grant into this node's memory.
    pub async fn reload_grants(&self) {
        let now = self.now();
        let rows = match self.store.running_grants(None, now.0).await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(error = %e, "overage grants could not be read");
                return;
            }
        };
        let mut by_budget: HashMap<CounterId, Vec<ActiveGrant>> = HashMap::new();
        for r in rows {
            let (Some(identity), Ok(amount)) = (CounterId::from_hex(&r.identity), r.amount.parse::<u128>())
            else {
                continue;
            };
            by_budget.entry(identity).or_default().push(ActiveGrant {
                id: r.id,
                amount,
                until: UtcMicros(r.expires_us),
            });
        }
        self.accounts.overlay().set_grants(by_budget);
    }
}

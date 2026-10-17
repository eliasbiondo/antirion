//! The cluster store's half of an organization's erasure, in one transaction.

use crate::StoreError;
use crate::erasure::{OWNED_PG, Tenant};

impl super::PgStore {
    pub(crate) async fn erase_ledger(&self, org: &str, before_us: i64) -> Result<u64, StoreError> {
        self.run(async move {
            let mut lease = self.pool.get().await?;
            let client = lease.client()?;
            let tx = client.transaction().await?;
            let mut n = tx
                .execute(
                    "DELETE FROM ledger_charges c USING ledger_lines l
                     WHERE l.request_id = c.request_id AND l.line = c.line AND l.admitted_us < $2
                       AND l.body::jsonb->>'organization' = $1",
                    &[&org, &before_us],
                )
                .await?;
            n += tx
                .execute(
                    "DELETE FROM ledger_lines WHERE admitted_us < $2 AND body::jsonb->>'organization' = $1",
                    &[&org, &before_us],
                )
                .await?;
            tx.commit().await?;
            Ok(n)
        })
        .await
    }

    pub(crate) async fn erase_tenant(&self, t: &Tenant) -> Result<u64, StoreError> {
        let org = t.organization.clone();
        let (teams, projects, keys, uids, scopes) = (
            t.teams.clone(),
            t.projects.clone(),
            t.keys.clone(),
            t.key_uids.clone(),
            t.scopes(),
        );
        self.run(async move {
            let mut lease = self.pool.get().await?;
            let client = lease.client()?;
            let tx = client.transaction().await?;
            let owned: [&(dyn tokio_postgres::types::ToSql + Sync); 4] = [&org, &teams, &projects, &keys];
            let mut n = 0;
            for table in ["budget_grants", "overage_requests", "budget_events"] {
                n += tx
                    .execute(
                        &format!(
                            "DELETE FROM budget_tallies WHERE identity IN (SELECT identity FROM {table} WHERE {OWNED_PG})"
                        ),
                        &owned,
                    )
                    .await?;
            }
            for table in ["budget_grants", "overage_requests", "budget_events"] {
                n += tx
                    .execute(&format!("DELETE FROM {table} WHERE {OWNED_PG}"), &owned)
                    .await?;
            }
            n += tx
                .execute(
                    "DELETE FROM ledger_charges c USING ledger_lines l
                     WHERE l.request_id = c.request_id AND l.line = c.line
                       AND l.body::jsonb->>'organization' = $1",
                    &[&org],
                )
                .await?;
            n += tx
                .execute(
                    "DELETE FROM ledger_lines WHERE body::jsonb->>'organization' = $1",
                    &[&org],
                )
                .await?;
            n += tx
                .execute(
                    "DELETE FROM use_marks WHERE kind = 'key' AND id = ANY($1)",
                    &[&uids],
                )
                .await?;
            n += tx
                .execute("DELETE FROM key_anomalies WHERE organization = $1", &[&org])
                .await?;
            for table in ["key_seen", "key_hours"] {
                n += tx
                    .execute(&format!("DELETE FROM {table} WHERE key_uid = ANY($1)"), &[&uids])
                    .await?;
            }
            for table in ["upstream_objects", "stored_responses", "response_links"] {
                n += tx
                    .execute(&format!("DELETE FROM {table} WHERE project = ANY($1)"), &[&projects])
                    .await?;
            }
            n += tx
                .execute("DELETE FROM token_bindings WHERE scope = ANY($1)", &[&scopes])
                .await?;
            n += tx
                .execute(
                    "DELETE FROM admin_sessions WHERE facts::jsonb->>'organization' = $1",
                    &[&org],
                )
                .await?;
            n += tx
                .execute("DELETE FROM log_rollups WHERE organization = $1", &[&org])
                .await?;
            n += tx
                .execute("DELETE FROM safety_events WHERE organization = $1", &[&org])
                .await?;
            tx.commit().await?;
            Ok(n)
        })
        .await
    }
}

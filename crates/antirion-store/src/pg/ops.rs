//! The operations both backends offer, on PostgreSQL: configuration, audit, node values, admin
//! tokens, sessions, token bindings and the ledger.

use antirion_core::UtcMicros;
use antirion_core::audit::AuditEntry;
use tokio_postgres::types::ToSql;

use super::{CONFIG_CHANNEL, PgStore};
use crate::{
    AdminTokenRecord, AuditPage, AuditQuery, GenerationRecord, LedgerRow, SessionRecord, StoreError,
    StoredConfig, TokenBindingRecord, to_i64,
};

type Param = Box<dyn ToSql + Sync + Send>;

fn token_of(r: &tokio_postgres::Row) -> Result<AdminTokenRecord, StoreError> {
    Ok(AdminTokenRecord {
        kid: r.try_get(0)?,
        user: r.try_get(1)?,
        verifier: r.try_get(2)?,
        description: r.try_get(3)?,
        created_at: r.try_get(4)?,
        expires_at: r.try_get(5)?,
        access: r.try_get(6)?,
        last_used_us: r.try_get(7)?,
    })
}

pub(crate) fn session_of(r: &tokio_postgres::Row) -> Result<SessionRecord, StoreError> {
    Ok(SessionRecord {
        id_digest: r.try_get(0)?,
        kid: r.try_get(1)?,
        created_us: r.try_get(2)?,
        last_used_us: r.try_get(3)?,
        user: r.try_get(4)?,
        facts: r.try_get(5)?,
    })
}

fn ledger_of(r: &tokio_postgres::Row) -> Result<LedgerRow, StoreError> {
    Ok(LedgerRow {
        request_id: r.try_get(0)?,
        line: u32::try_from(r.try_get::<_, i32>(1)?).unwrap_or(0),
        admitted_us: r.try_get(2)?,
        body: r.try_get(3)?,
        charges: None,
    })
}

impl PgStore {
    pub(crate) async fn load_config(&self) -> Result<Option<StoredConfig>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let row = lease
                .client()?
                .query_opt(
                    "SELECT generation, document, metadata FROM config WHERE id = 1",
                    &[],
                )
                .await?;
            row.map(|r| {
                Ok(StoredConfig {
                    generation: u64::try_from(r.try_get::<_, i64>(0)?).unwrap_or(0),
                    document: r.try_get(1)?,
                    metadata: r.try_get(2)?,
                })
            })
            .transpose()
        })
        .await
    }

    /// The committed generation, without the document.
    pub async fn head(&self) -> Result<u64, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let row = lease
                .client()?
                .query_opt("SELECT generation FROM config WHERE id = 1", &[])
                .await?;
            Ok(row
                .map(|r| r.try_get::<_, i64>(0))
                .transpose()?
                .map_or(0, |g| u64::try_from(g).unwrap_or(0)))
        })
        .await
    }

    /// Commits `config` only when the stored generation is the one before it; otherwise another
    /// node committed first and nothing is written ([`StoreError::Conflict`]).
    pub(crate) async fn commit_config(
        &self,
        config: StoredConfig,
        history: GenerationRecord,
        audit: Option<AuditEntry>,
    ) -> Result<(), StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let client = lease.client()?;
            let tx = client.transaction().await?;
            let generation = to_i64(config.generation);
            let expected = generation - 1;
            let written = tx
                .execute(
                    "INSERT INTO config (id, generation, document, metadata) VALUES (1, $1, $2, $3)
                     ON CONFLICT (id) DO UPDATE SET generation = excluded.generation,
                         document = excluded.document, metadata = excluded.metadata
                     WHERE config.generation = $4",
                    &[&generation, &config.document, &config.metadata, &expected],
                )
                .await?;
            if written == 0 {
                return Err(StoreError::Conflict);
            }
            tx.execute(
                "INSERT INTO generations (generation, created_at, actor, source, summary)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (generation) DO UPDATE SET created_at = excluded.created_at,
                     actor = excluded.actor, source = excluded.source, summary = excluded.summary",
                &[
                    &to_i64(history.generation),
                    &history.created_at,
                    &history.actor,
                    &history.source,
                    &history.summary,
                ],
            )
            .await?;
            if let Some(entry) = audit {
                insert_audit(&tx, &entry).await?;
            }
            tx.execute(
                "SELECT pg_notify($1, $2)",
                &[&CONFIG_CHANNEL, &config.generation.to_string()],
            )
            .await?;
            tx.commit().await?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn append_audit(&self, entry: AuditEntry) -> Result<(), StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            insert_audit(lease.client()?, &entry).await
        })
        .await
    }

    pub(crate) async fn audit(&self, q: AuditQuery) -> Result<AuditPage, StoreError> {
        self.run(async {
            let mut sql = String::from("SELECT seq, entry FROM audit WHERE TRUE");
            let mut params: Vec<Param> = Vec::new();
            let mut text = |column: &str, value: &Option<String>, sql: &mut String| {
                if let Some(v) = value {
                    let placeholder = format!("${}", params.len() + 1);
                    let (clause, value) = crate::text_condition(column, v, &placeholder);
                    params.push(Box::new(value));
                    sql.push_str(&clause);
                }
            };
            text("actor_id", &q.actor, &mut sql);
            text("resource", &q.resource, &mut sql);
            text("action", &q.action, &mut sql);
            text("outcome", &q.outcome, &mut sql);
            text("kind", &q.kind, &mut sql);
            for (cond, value) in [
                ("time_us >=", q.from.map(|t| t.0)),
                ("time_us <=", q.to.map(|t| t.0)),
                ("seq <", q.before),
            ] {
                if let Some(v) = value {
                    params.push(Box::new(v));
                    sql.push_str(&format!(" AND {cond} ${}", params.len()));
                }
            }
            let limit = i64::try_from(q.limit).unwrap_or(i64::MAX);
            params.push(Box::new(limit.saturating_add(1)));
            sql.push_str(&format!(" ORDER BY seq DESC LIMIT ${}", params.len()));
            let refs: Vec<&(dyn ToSql + Sync)> =
                params.iter().map(|p| p.as_ref() as &(dyn ToSql + Sync)).collect();
            let mut lease = self.pool.get().await?;
            let rows = lease.client()?.query(sql.as_str(), &refs).await?;
            let mut items = Vec::new();
            let mut last = None;
            let mut more = false;
            for r in rows {
                if items.len() == q.limit {
                    more = true;
                    break;
                }
                let seq: i64 = r.try_get(0)?;
                let entry: String = r.try_get(1)?;
                items.push(serde_json::from_str::<AuditEntry>(&entry)?);
                last = Some(seq);
            }
            Ok(AuditPage {
                items,
                next: if more { last } else { None },
            })
        })
        .await
    }

    pub(crate) async fn audit_entry(&self, id: String) -> Result<Option<AuditEntry>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let row = lease
                .client()?
                .query_opt("SELECT entry FROM audit WHERE id = $1", &[&id])
                .await?;
            row.map(|r| Ok(serde_json::from_str(&r.try_get::<_, String>(0)?)?))
                .transpose()
        })
        .await
    }

    pub(crate) async fn execute(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> Result<u64, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            Ok(lease.client()?.execute(sql, params).await?)
        })
        .await
    }

    /// The rows of one query.
    pub(crate) async fn rows(
        &self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<tokio_postgres::Row>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            Ok(lease.client()?.query(sql, params).await?)
        })
        .await
    }

    pub(crate) async fn get_or_init(&self, key: &str, init: Vec<u8>) -> Result<Vec<u8>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let c = lease.client()?;
            c.execute(
                "INSERT INTO kv (key, value) VALUES ($1, $2) ON CONFLICT (key) DO NOTHING",
                &[&key, &init],
            )
            .await?;
            let row = c
                .query_one("SELECT value FROM kv WHERE key = $1", &[&key])
                .await?;
            Ok(row.try_get(0)?)
        })
        .await
    }

    pub(crate) async fn put_value(&self, key: &str, value: Vec<u8>) -> Result<(), StoreError> {
        self.execute(
            "INSERT INTO kv (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            &[&key, &value],
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn put_admin_token(&self, rec: AdminTokenRecord) -> Result<(), StoreError> {
        self.execute(
            "INSERT INTO admin_tokens (kid, user_name, verifier, description, created_at, expires_at, access)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            &[
                &rec.kid,
                &rec.user,
                &rec.verifier,
                &rec.description,
                &rec.created_at,
                &rec.expires_at,
                &rec.access,
            ],
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn admin_token(&self, kid: String) -> Result<Option<AdminTokenRecord>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let row = lease
                .client()?
                .query_opt(
                    "SELECT kid, user_name, verifier, description, created_at, expires_at, access, last_used_us
                     FROM admin_tokens WHERE kid = $1",
                    &[&kid],
                )
                .await?;
            row.as_ref().map(token_of).transpose()
        })
        .await
    }

    pub(crate) async fn admin_tokens_of(&self, user: String) -> Result<Vec<AdminTokenRecord>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let rows = lease
                .client()?
                .query(
                    "SELECT kid, user_name, verifier, description, created_at, expires_at, access, last_used_us
                     FROM admin_tokens WHERE user_name = $1 ORDER BY kid",
                    &[&user],
                )
                .await?;
            rows.iter().map(token_of).collect()
        })
        .await
    }

    pub(crate) async fn put_session(&self, rec: SessionRecord) -> Result<(), StoreError> {
        self.execute(
            "INSERT INTO admin_sessions (id_digest, kid, created_us, last_used_us, user_name, facts)
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &rec.id_digest,
                &rec.kid,
                &rec.created_us,
                &rec.last_used_us,
                &rec.user,
                &rec.facts,
            ],
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn session(&self, id_digest: String) -> Result<Option<SessionRecord>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let row = lease
                .client()?
                .query_opt(
                    "SELECT id_digest, kid, created_us, last_used_us, user_name, facts FROM admin_sessions
                     WHERE id_digest = $1",
                    &[&id_digest],
                )
                .await?;
            row.as_ref().map(session_of).transpose()
        })
        .await
    }

    pub(crate) async fn put_token_binding(&self, rec: TokenBindingRecord) -> Result<(), StoreError> {
        self.execute(
            "INSERT INTO token_bindings (id, kid, role, scope, created_at) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (id) DO NOTHING",
            &[&rec.id, &rec.kid, &rec.role, &rec.scope, &rec.created_at],
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn token_bindings(
        &self,
        kid: Option<String>,
    ) -> Result<Vec<TokenBindingRecord>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let rows = lease
                .client()?
                .query(
                    "SELECT id, kid, role, scope, created_at FROM token_bindings
                     WHERE $1::text IS NULL OR kid = $1 ORDER BY id",
                    &[&kid],
                )
                .await?;
            rows.iter()
                .map(|r| {
                    Ok(TokenBindingRecord {
                        id: r.try_get(0)?,
                        kid: r.try_get(1)?,
                        role: r.try_get(2)?,
                        scope: r.try_get(3)?,
                        created_at: r.try_get(4)?,
                    })
                })
                .collect()
        })
        .await
    }

    /// Appends ledger lines in one statement; lines already stored are kept as they are.
    pub(crate) async fn append_ledger(&self, rows: Vec<LedgerRow>) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let ids: Vec<&str> = rows.iter().map(|r| r.request_id.as_str()).collect();
        let lines: Vec<i32> = rows
            .iter()
            .map(|r| i32::try_from(r.line).unwrap_or(i32::MAX))
            .collect();
        let admitted: Vec<i64> = rows.iter().map(|r| r.admitted_us).collect();
        let bodies: Vec<&str> = rows.iter().map(|r| r.body.as_str()).collect();
        self.execute(
            "WITH ins AS (
                INSERT INTO ledger_lines (request_id, line, admitted_us, body, node_id)
                SELECT r.id, r.line, r.admitted, r.body, $5
                FROM unnest($1::text[], $2::int4[], $3::int8[], $4::text[]) AS r(id, line, admitted, body)
                ON CONFLICT (request_id, line) DO NOTHING
                RETURNING request_id, line, admitted_us, body)
             INSERT INTO ledger_charges (counter, admitted_us, request_id, line, amount)
             SELECT b->>'counter', ins.admitted_us, ins.request_id, ins.line, (b->>'amount')::numeric
             FROM ins,
                  jsonb_array_elements(CASE WHEN jsonb_typeof(ins.body::jsonb->'budgets') = 'array'
                                            THEN ins.body::jsonb->'budgets' ELSE '[]'::jsonb END) b
             WHERE b ? 'counter' AND b ? 'amount'
             ON CONFLICT DO NOTHING",
            &[&ids, &lines, &admitted, &bodies, &self.node],
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn ledger_between(&self, from: i64, to: i64) -> Result<Vec<LedgerRow>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let rows = lease
                .client()?
                .query(
                    "SELECT request_id, line, admitted_us, body FROM ledger_lines
                     WHERE admitted_us >= $1 AND admitted_us < $2
                     ORDER BY admitted_us, request_id, line",
                    &[&from, &to],
                )
                .await?;
            rows.iter().map(ledger_of).collect()
        })
        .await
    }

    pub(crate) async fn ledger_request(&self, request_id: &str) -> Result<Vec<LedgerRow>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let rows = lease
                .client()?
                .query(
                    "SELECT request_id, line, admitted_us, body FROM ledger_lines
                     WHERE request_id = $1 ORDER BY line",
                    &[&request_id],
                )
                .await?;
            rows.iter().map(ledger_of).collect()
        })
        .await
    }

    pub(crate) async fn ledger_charged(&self, counter: &str, from: i64, to: i64) -> Result<i128, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let row = lease
                .client()?
                .query_one(
                    "SELECT COALESCE(SUM(amount), 0)::text FROM ledger_charges
                     WHERE counter = $1 AND admitted_us >= $2 AND admitted_us < $3",
                    &[&counter, &from, &to],
                )
                .await?;
            let total: String = row.try_get(0)?;
            Ok(total.parse::<i128>().unwrap_or(0))
        })
        .await
    }

    pub(crate) async fn ledger_charged_slots(
        &self,
        counter: &str,
        from: i64,
        sub_us: i64,
    ) -> Result<Vec<(i64, i128)>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let rows = lease
                .client()?
                .query(
                    "SELECT admitted_us / $3 AS slot, SUM(amount)::text FROM ledger_charges
                     WHERE counter = $1 AND admitted_us >= $2 GROUP BY slot ORDER BY slot",
                    &[&counter, &from, &sub_us],
                )
                .await?;
            rows.iter()
                .map(|r| {
                    let slot: i64 = r.try_get(0)?;
                    let total: String = r.try_get(1)?;
                    Ok((slot, total.parse::<i128>().unwrap_or(0)))
                })
                .collect()
        })
        .await
    }

    pub(crate) async fn clear_ledger(&self) -> Result<(), StoreError> {
        self.execute("DELETE FROM ledger_lines", &[]).await?;
        self.execute("DELETE FROM ledger_charges", &[]).await.map(|_| ())
    }

    /// Ledger lines stored after `after_us` (store time, microseconds), oldest first, and the
    /// store time of the newest one returned.
    pub async fn ledger_since(&self, after_us: i64, limit: i64) -> Result<(Vec<LedgerRow>, i64), StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let rows = lease
                .client()?
                .query(
                    "SELECT request_id, line, admitted_us, body,
                         floor(extract(epoch FROM inserted_at) * 1000000)::int8
                     FROM ledger_lines
                     WHERE inserted_at > to_timestamp($1::int8::float8 / 1000000.0)
                     ORDER BY inserted_at LIMIT $2",
                    &[&after_us, &limit],
                )
                .await?;
            let mut newest = after_us;
            let mut out = Vec::with_capacity(rows.len());
            for r in &rows {
                out.push(ledger_of(r)?);
                newest = newest.max(r.try_get::<_, i64>(4)?);
            }
            Ok((out, newest))
        })
        .await
    }

    /// The store's clock, in microseconds since the epoch.
    pub async fn now_us(&self) -> Result<i64, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let row = lease
                .client()?
                .query_one(
                    "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000000)::int8",
                    &[],
                )
                .await?;
            Ok(row.try_get(0)?)
        })
        .await
    }
}

async fn insert_audit(c: &impl tokio_postgres::GenericClient, e: &AuditEntry) -> Result<(), StoreError> {
    let time_us = UtcMicros::parse_rfc3339(&e.time).map(|t| t.0).unwrap_or(0);
    c.execute(
        "INSERT INTO audit (id, time, time_us, actor_id, action, resource, outcome, entry, kind)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        &[
            &e.id,
            &e.time,
            &time_us,
            &e.actor.id,
            &e.action,
            &e.resource,
            &e.outcome.as_str(),
            &serde_json::to_string(e)?,
            &e.kind.as_str(),
        ],
    )
    .await?;
    Ok(())
}

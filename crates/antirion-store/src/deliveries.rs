//! The delivery log of webhooks, alert channels and audit sinks (one row per attempt), the depth of
//! each node's delivery queues, and the position of each audit sink in the audit log.

use antirion_core::audit::AuditEntry;
use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreError};

/// One delivery attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryRow {
    pub event_id: String,
    /// `webhook`, `channel` or `audit_sink`.
    pub target_kind: String,
    pub target: String,
    pub event_type: String,
    /// 1 for the first attempt of an event.
    pub attempt: i32,
    pub time_us: i64,
    pub status_code: Option<i32>,
    /// `delivered`, `failed`, `timeout` or `dropped`.
    pub result: String,
    /// How many entries the attempt carried (audit sinks send batches).
    pub items: i64,
}

/// Filters of a delivery listing; `before` is the position to continue from.
#[derive(Clone, Debug, Default)]
pub struct DeliveryQuery {
    pub target_kind: Option<String>,
    /// Leaves out audit sinks (the alert delivery log of webhooks and channels).
    pub alerts_only: bool,
    pub target: Option<String>,
    pub event_type: Option<String>,
    pub result: Option<String>,
    pub since_us: i64,
    pub before: Option<i64>,
    pub limit: usize,
}

/// The queue of one target on one node, as last reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueRow {
    pub node_id: String,
    pub target_kind: String,
    pub target: String,
    pub depth: i64,
    pub oldest_us: Option<i64>,
    pub updated_us: i64,
}

const DELIVERY: &str =
    "seq, event_id, target_kind, target, event_type, attempt, time_us, status_code, result, items";

fn delivery(r: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, DeliveryRow)> {
    Ok((
        r.get(0)?,
        DeliveryRow {
            event_id: r.get(1)?,
            target_kind: r.get(2)?,
            target: r.get(3)?,
            event_type: r.get(4)?,
            attempt: r.get(5)?,
            time_us: r.get(6)?,
            status_code: r.get(7)?,
            result: r.get(8)?,
            items: r.get(9)?,
        },
    ))
}

fn pg_delivery(r: &tokio_postgres::Row) -> Result<(i64, DeliveryRow), StoreError> {
    Ok((
        r.try_get(0)?,
        DeliveryRow {
            event_id: r.try_get(1)?,
            target_kind: r.try_get(2)?,
            target: r.try_get(3)?,
            event_type: r.try_get(4)?,
            attempt: r.try_get(5)?,
            time_us: r.try_get(6)?,
            status_code: r.try_get(7)?,
            result: r.try_get(8)?,
            items: r.try_get(9)?,
        },
    ))
}

/// The newest-first page of `rows` (one more than `limit` was read) and the position after it.
fn page<T>(mut rows: Vec<(i64, T)>, limit: usize) -> (Vec<(i64, T)>, Option<i64>) {
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = more.then(|| rows.last().map(|r| r.0)).flatten();
    (rows, next)
}

impl Store {
    pub async fn record_delivery(&self, row: DeliveryRow) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        "INSERT INTO deliveries
                            (event_id, target_kind, target, event_type, attempt, time_us, status_code, result, items)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            row.event_id,
                            row.target_kind,
                            row.target,
                            row.event_type,
                            row.attempt,
                            row.time_us,
                            row.status_code,
                            row.result,
                            row.items
                        ],
                    )?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    "INSERT INTO deliveries
                        (event_id, target_kind, target, event_type, attempt, time_us, status_code, result, items)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                    &[
                        &row.event_id,
                        &row.target_kind,
                        &row.target,
                        &row.event_type,
                        &row.attempt,
                        &row.time_us,
                        &row.status_code,
                        &row.result,
                        &row.items,
                    ],
                )
                .await
                .map(|_| ()),
        }
    }

    /// Attempts matching `q`, newest first, and the position to continue from.
    pub async fn deliveries(
        &self,
        q: DeliveryQuery,
    ) -> Result<(Vec<(i64, DeliveryRow)>, Option<i64>), StoreError> {
        let limit = q.limit;
        let read = i64::try_from(limit).unwrap_or(i64::MAX).saturating_add(1);
        let before = q.before.unwrap_or(i64::MAX);
        let rows = match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut stmt = c.prepare(&format!(
                        "SELECT {DELIVERY} FROM deliveries
                         WHERE time_us >= ?1 AND seq < ?2
                           AND (?3 IS NULL OR target_kind = ?3) AND (?4 IS NULL OR target = ?4)
                           AND (?5 IS NULL OR event_type = ?5) AND (?6 IS NULL OR result = ?6)
                           AND (?8 = 0 OR target_kind <> 'audit_sink')
                         ORDER BY seq DESC LIMIT ?7"
                    ))?;
                    let rows = stmt.query_map(
                        params![
                            q.since_us,
                            before,
                            q.target_kind,
                            q.target,
                            q.event_type,
                            q.result,
                            read,
                            i64::from(q.alerts_only)
                        ],
                        delivery,
                    )?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await?
            }
            Store::Postgres(p) => p
                .rows(
                    &format!(
                        "SELECT {DELIVERY} FROM deliveries
                         WHERE time_us >= $1 AND seq < $2
                           AND ($3::text IS NULL OR target_kind = $3) AND ($4::text IS NULL OR target = $4)
                           AND ($5::text IS NULL OR event_type = $5) AND ($6::text IS NULL OR result = $6)
                           AND (NOT $8 OR target_kind <> 'audit_sink')
                         ORDER BY seq DESC LIMIT $7"
                    ),
                    &[
                        &q.since_us,
                        &before,
                        &q.target_kind,
                        &q.target,
                        &q.event_type,
                        &q.result,
                        &read,
                        &q.alerts_only,
                    ],
                )
                .await?
                .iter()
                .map(pg_delivery)
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(page(rows, limit))
    }

    /// Reports this node's queue for one target.
    pub async fn report_queue(&self, row: QueueRow) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        "INSERT INTO delivery_queues (node_id, target_kind, target, depth, oldest_us, updated_us)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                         ON CONFLICT (node_id, target_kind, target) DO UPDATE SET depth = excluded.depth,
                            oldest_us = excluded.oldest_us, updated_us = excluded.updated_us",
                        params![
                            row.node_id,
                            row.target_kind,
                            row.target,
                            row.depth,
                            row.oldest_us,
                            row.updated_us
                        ],
                    )?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    "INSERT INTO delivery_queues (node_id, target_kind, target, depth, oldest_us, updated_us)
                     VALUES ($1, $2, $3, $4, $5, $6)
                     ON CONFLICT (node_id, target_kind, target) DO UPDATE SET depth = excluded.depth,
                        oldest_us = excluded.oldest_us, updated_us = excluded.updated_us",
                    &[
                        &row.node_id,
                        &row.target_kind,
                        &row.target,
                        &row.depth,
                        &row.oldest_us,
                        &row.updated_us,
                    ],
                )
                .await
                .map(|_| ()),
        }
    }

    /// The queues every node reported at or after `since_us`.
    pub async fn queues(&self, since_us: i64) -> Result<Vec<QueueRow>, StoreError> {
        const SQL: &str =
            "SELECT node_id, target_kind, target, depth, oldest_us, updated_us FROM delivery_queues";
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut stmt = c.prepare(&format!("{SQL} WHERE updated_us >= ?1"))?;
                    let rows = stmt.query_map([since_us], |r| {
                        Ok(QueueRow {
                            node_id: r.get(0)?,
                            target_kind: r.get(1)?,
                            target: r.get(2)?,
                            depth: r.get(3)?,
                            oldest_us: r.get(4)?,
                            updated_us: r.get(5)?,
                        })
                    })?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => p
                .rows(&format!("{SQL} WHERE updated_us >= $1"), &[&since_us])
                .await?
                .iter()
                .map(|r| {
                    Ok(QueueRow {
                        node_id: r.try_get(0)?,
                        target_kind: r.try_get(1)?,
                        target: r.try_get(2)?,
                        depth: r.try_get(3)?,
                        oldest_us: r.try_get(4)?,
                        updated_us: r.try_get(5)?,
                    })
                })
                .collect(),
        }
    }

    /// The node value stored under `key`, when there is one.
    pub async fn value(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let key = key.to_owned();
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(
                        c.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
                            .optional()?,
                    )
                })
                .await
            }
            Store::Postgres(p) => Ok(p
                .rows("SELECT value FROM kv WHERE key = $1", &[&key])
                .await?
                .first()
                .map(|r| r.try_get(0))
                .transpose()?),
        }
    }

    /// The audit position an audit sink delivered up to, when it has one.
    pub async fn sink_cursor(&self, sink: &str) -> Result<Option<i64>, StoreError> {
        let sink = sink.to_owned();
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.query_row(
                        "SELECT last_seq FROM audit_sink_cursors WHERE sink = ?1",
                        [sink],
                        |r| r.get(0),
                    )
                    .optional()?)
                })
                .await
            }
            Store::Postgres(p) => Ok(p
                .rows(
                    "SELECT last_seq FROM audit_sink_cursors WHERE sink = $1",
                    &[&sink],
                )
                .await?
                .first()
                .map(|r| r.try_get(0))
                .transpose()?),
        }
    }

    pub async fn set_sink_cursor(&self, sink: &str, seq: i64) -> Result<(), StoreError> {
        let sink = sink.to_owned();
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    c.execute(
                        "INSERT INTO audit_sink_cursors (sink, last_seq) VALUES (?1, ?2)
                         ON CONFLICT (sink) DO UPDATE SET last_seq = excluded.last_seq",
                        params![sink, seq],
                    )?;
                    Ok(())
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    "INSERT INTO audit_sink_cursors (sink, last_seq) VALUES ($1, $2)
                     ON CONFLICT (sink) DO UPDATE SET last_seq = excluded.last_seq",
                    &[&sink, &seq],
                )
                .await
                .map(|_| ()),
        }
    }

    /// The newest audit position, 0 when the log is empty.
    pub async fn audit_head(&self) -> Result<i64, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(|c| Ok(c.query_row("SELECT COALESCE(MAX(seq), 0) FROM audit", [], |r| r.get(0))?))
                    .await
            }
            Store::Postgres(p) => Ok(p
                .rows("SELECT COALESCE(MAX(seq), 0)::bigint FROM audit", &[])
                .await?
                .first()
                .map(|r| r.try_get(0))
                .transpose()?
                .unwrap_or(0)),
        }
    }

    /// Up to `limit` audit entries after position `after`, oldest first, with their positions and
    /// times.
    pub async fn audit_after(
        &self,
        after: i64,
        limit: usize,
    ) -> Result<Vec<(i64, i64, AuditEntry)>, StoreError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows: Vec<(i64, i64, String)> = match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let mut stmt = c.prepare(
                        "SELECT seq, time_us, entry FROM audit WHERE seq > ?1 ORDER BY seq LIMIT ?2",
                    )?;
                    let rows =
                        stmt.query_map(params![after, limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await?
            }
            Store::Postgres(p) => p
                .rows(
                    "SELECT seq, time_us, entry FROM audit WHERE seq > $1 ORDER BY seq LIMIT $2",
                    &[&after, &limit],
                )
                .await?
                .iter()
                .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
                .collect::<Result<Vec<_>, StoreError>>()?,
        };
        rows.into_iter()
            .map(|(seq, t, e)| Ok((seq, t, serde_json::from_str(&e)?)))
            .collect()
    }

    /// How many audit entries follow position `after`, and the time of the oldest of them.
    pub async fn audit_backlog(&self, after: i64) -> Result<(i64, Option<i64>), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.query_row(
                        "SELECT COUNT(*), MIN(time_us) FROM audit WHERE seq > ?1",
                        [after],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?)
                })
                .await
            }
            Store::Postgres(p) => {
                let rows = p
                    .rows(
                        "SELECT COUNT(*)::bigint, MIN(time_us) FROM audit WHERE seq > $1",
                        &[&after],
                    )
                    .await?;
                match rows.first() {
                    Some(r) => Ok((r.try_get(0)?, r.try_get(1)?)),
                    None => Ok((0, None)),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_keep_the_limit_and_point_after_the_last_row() {
        let rows: Vec<(i64, ())> = (0..5).rev().map(|n| (n, ())).collect();
        let (got, next) = page(rows.clone(), 3);
        assert_eq!(got.len(), 3);
        assert_eq!(next, Some(2));
        let (got, next) = page(rows, 5);
        assert_eq!(got.len(), 5);
        assert_eq!(next, None);
    }
}

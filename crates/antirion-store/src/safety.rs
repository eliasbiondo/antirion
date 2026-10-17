//! Safety events and guardrail statistics. An event is one non-pass verdict of a guardrail (or a
//! routing block) on a request, kept beside the request log with the same retention: in `logs.db`
//! in standalone mode, in the shared PostgreSQL database in a cluster. Statistics are counts and a
//! latency sketch per guardrail, hour and node; each node writes only its own rows.

use rusqlite::{Connection, OptionalExtension};

use crate::logs::sql::{Arg, Sql, like_escape, scope_clause};
use crate::logs::{ScopeRef, SketchMerge};
use crate::{Store, StoreError};

/// The SQLite tables (in `logs.db`).
pub(crate) const SQLITE_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS safety_events (
        id TEXT PRIMARY KEY, time_us INTEGER NOT NULL, log_id TEXT NOT NULL, request_id TEXT NOT NULL,
        node_id TEXT NOT NULL, guardrail TEXT NOT NULL, guardrail_type TEXT NOT NULL, stage TEXT NOT NULL,
        action TEXT NOT NULL, category TEXT NOT NULL, severity TEXT NOT NULL, confidence REAL NOT NULL,
        score REAL, entities TEXT NOT NULL, spans TEXT, fingerprint TEXT NOT NULL, organization TEXT,
        team TEXT, project TEXT, key_name TEXT, end_user TEXT, model TEXT, provider TEXT, latency_ms REAL,
        review_state TEXT NOT NULL, reviewed_by TEXT, reviewed_us INTEGER, note TEXT)",
    "CREATE INDEX IF NOT EXISTS safety_events_time ON safety_events (time_us, id)",
    "CREATE INDEX IF NOT EXISTS safety_events_fingerprint ON safety_events (fingerprint, time_us)",
    "CREATE INDEX IF NOT EXISTS safety_events_guardrail ON safety_events (guardrail, time_us)",
    "CREATE TABLE IF NOT EXISTS guardrail_hours (
        guardrail TEXT NOT NULL, hour_us INTEGER NOT NULL, node_id TEXT NOT NULL,
        evaluations INTEGER NOT NULL, hits INTEGER NOT NULL, blocks INTEGER NOT NULL,
        redactions INTEGER NOT NULL, flags INTEGER NOT NULL, errors INTEGER NOT NULL,
        latency BLOB NOT NULL, PRIMARY KEY (guardrail, hour_us, node_id))",
];

/// The PostgreSQL migration.
pub(crate) const PG_MIGRATION: &str = "CREATE TABLE IF NOT EXISTS safety_events (
        id text PRIMARY KEY, time_us bigint NOT NULL, log_id text NOT NULL, request_id text NOT NULL,
        node_id text NOT NULL, guardrail text NOT NULL, guardrail_type text NOT NULL, stage text NOT NULL,
        action text NOT NULL, category text NOT NULL, severity text NOT NULL,
        confidence double precision NOT NULL, score double precision, entities text NOT NULL, spans text,
        fingerprint text NOT NULL, organization text, team text, project text, key_name text,
        end_user text, model text, provider text, latency_ms double precision,
        review_state text NOT NULL, reviewed_by text, reviewed_us bigint, note text);
     CREATE INDEX IF NOT EXISTS safety_events_time ON safety_events (time_us, id);
     CREATE INDEX IF NOT EXISTS safety_events_fingerprint ON safety_events (fingerprint, time_us);
     CREATE INDEX IF NOT EXISTS safety_events_guardrail ON safety_events (guardrail, time_us);
     CREATE TABLE IF NOT EXISTS guardrail_hours (
        guardrail text NOT NULL, hour_us bigint NOT NULL, node_id text NOT NULL,
        evaluations bigint NOT NULL, hits bigint NOT NULL, blocks bigint NOT NULL,
        redactions bigint NOT NULL, flags bigint NOT NULL, errors bigint NOT NULL,
        latency bytea NOT NULL, PRIMARY KEY (guardrail, hour_us, node_id));";

/// One safety event as the store keeps it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SafetyEventRow {
    pub id: String,
    pub time_us: i64,
    pub log_id: String,
    pub request_id: String,
    pub node_id: String,
    pub guardrail: String,
    pub guardrail_type: String,
    pub stage: String,
    pub action: String,
    pub category: String,
    pub severity: String,
    pub confidence: f64,
    pub score: Option<f64>,
    /// JSON `[{type, count}]`.
    pub entities: String,
    /// JSON `[{start, end, entity}]`, when the check located its matches.
    pub spans: Option<String>,
    pub fingerprint: String,
    pub organization: Option<String>,
    pub team: Option<String>,
    pub project: Option<String>,
    pub key: Option<String>,
    pub end_user: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub latency_ms: Option<f64>,
    pub review_state: String,
    pub reviewed_by: Option<String>,
    pub reviewed_us: Option<i64>,
    pub note: Option<String>,
}

pub(crate) const COLUMNS: &str = "id, time_us, log_id, request_id, node_id, guardrail, guardrail_type, stage, action, \
     category, severity, confidence, score, entities, spans, fingerprint, organization, team, project, \
     key_name, end_user, model, provider, latency_ms, review_state, reviewed_by, reviewed_us, note";

const COLUMN_COUNT: usize = 28;

/// A column an event list filters with a list of accepted values.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SafetyColumn {
    Id,
    Category,
    Severity,
    Action,
    Guardrail,
    ReviewState,
    Fingerprint,
    Organization,
    Team,
    Project,
    Key,
    Model,
    Provider,
    EndUser,
}

impl SafetyColumn {
    fn sql(self) -> &'static str {
        match self {
            SafetyColumn::Id => "id",
            SafetyColumn::Category => "category",
            SafetyColumn::Severity => "severity",
            SafetyColumn::Action => "action",
            SafetyColumn::Guardrail => "guardrail",
            SafetyColumn::ReviewState => "review_state",
            SafetyColumn::Fingerprint => "fingerprint",
            SafetyColumn::Organization => "organization",
            SafetyColumn::Team => "team",
            SafetyColumn::Project => "project",
            SafetyColumn::Key => "key_name",
            SafetyColumn::Model => "model",
            SafetyColumn::Provider => "provider",
            SafetyColumn::EndUser => "end_user",
        }
    }
}

/// What an event query selects; every member narrows it.
#[derive(Clone, Debug, Default)]
pub struct SafetyFilter {
    pub from_us: Option<i64>,
    pub to_us: Option<i64>,
    /// Each column matches one of its values; the columns are combined with AND.
    pub any_of: Vec<(SafetyColumn, Vec<String>)>,
    /// A case-insensitive substring of the event id or the request id.
    pub q: Option<String>,
    /// Events of these scopes only; `None` for every event.
    pub scopes: Option<Vec<ScopeRef>>,
}

/// One guardrail's counts in one hour on one node.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GuardrailHour {
    pub guardrail: String,
    pub hour_us: i64,
    pub node_id: String,
    pub evaluations: i64,
    pub hits: i64,
    pub blocks: i64,
    pub redactions: i64,
    pub flags: i64,
    pub errors: i64,
    /// An encoded latency sketch (milliseconds).
    pub latency: Vec<u8>,
}

impl GuardrailHour {
    pub(crate) fn add(&mut self, other: &GuardrailHour, merge: SketchMerge) {
        self.evaluations = self.evaluations.saturating_add(other.evaluations);
        self.hits = self.hits.saturating_add(other.hits);
        self.blocks = self.blocks.saturating_add(other.blocks);
        self.redactions = self.redactions.saturating_add(other.redactions);
        self.flags = self.flags.saturating_add(other.flags);
        self.errors = self.errors.saturating_add(other.errors);
        self.latency = merge(&self.latency, &other.latency);
    }
}

pub(crate) const HOUR_COLUMNS: &str =
    "guardrail, hour_us, node_id, evaluations, hits, blocks, redactions, flags, errors, latency";

pub(crate) const HOUR_UPSERT: &str =
    "INSERT INTO guardrail_hours (guardrail, hour_us, node_id, evaluations, hits, blocks, redactions, flags,
        errors, latency) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
     ON CONFLICT (guardrail, hour_us, node_id) DO UPDATE SET evaluations = excluded.evaluations,
        hits = excluded.hits, blocks = excluded.blocks, redactions = excluded.redactions,
        flags = excluded.flags, errors = excluded.errors, latency = excluded.latency";

pub(crate) const HOUR_FIND: &str =
    "SELECT guardrail, hour_us, node_id, evaluations, hits, blocks, redactions, flags, errors, latency
     FROM guardrail_hours WHERE guardrail = $1 AND hour_us = $2 AND node_id = $3";

fn lite(sql: &str) -> String {
    sql.replace('$', "?")
}

pub(crate) fn args(r: &SafetyEventRow) -> Vec<Option<Arg>> {
    let t = |v: &Option<String>| v.clone().map(Arg::Text);
    vec![
        Some(Arg::Text(r.id.clone())),
        Some(Arg::Int(r.time_us)),
        Some(Arg::Text(r.log_id.clone())),
        Some(Arg::Text(r.request_id.clone())),
        Some(Arg::Text(r.node_id.clone())),
        Some(Arg::Text(r.guardrail.clone())),
        Some(Arg::Text(r.guardrail_type.clone())),
        Some(Arg::Text(r.stage.clone())),
        Some(Arg::Text(r.action.clone())),
        Some(Arg::Text(r.category.clone())),
        Some(Arg::Text(r.severity.clone())),
        Some(Arg::Real(r.confidence)),
        r.score.map(Arg::Real),
        Some(Arg::Text(r.entities.clone())),
        t(&r.spans),
        Some(Arg::Text(r.fingerprint.clone())),
        t(&r.organization),
        t(&r.team),
        t(&r.project),
        t(&r.key),
        t(&r.end_user),
        t(&r.model),
        t(&r.provider),
        r.latency_ms.map(Arg::Real),
        Some(Arg::Text(r.review_state.clone())),
        t(&r.reviewed_by),
        r.reviewed_us.map(Arg::Int),
        t(&r.note),
    ]
}

fn sqlite_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SafetyEventRow> {
    Ok(SafetyEventRow {
        id: r.get(0)?,
        time_us: r.get(1)?,
        log_id: r.get(2)?,
        request_id: r.get(3)?,
        node_id: r.get(4)?,
        guardrail: r.get(5)?,
        guardrail_type: r.get(6)?,
        stage: r.get(7)?,
        action: r.get(8)?,
        category: r.get(9)?,
        severity: r.get(10)?,
        confidence: r.get(11)?,
        score: r.get(12)?,
        entities: r.get(13)?,
        spans: r.get(14)?,
        fingerprint: r.get(15)?,
        organization: r.get(16)?,
        team: r.get(17)?,
        project: r.get(18)?,
        key: r.get(19)?,
        end_user: r.get(20)?,
        model: r.get(21)?,
        provider: r.get(22)?,
        latency_ms: r.get(23)?,
        review_state: r.get(24)?,
        reviewed_by: r.get(25)?,
        reviewed_us: r.get(26)?,
        note: r.get(27)?,
    })
}

pub(crate) fn pg_row(r: &tokio_postgres::Row) -> Result<SafetyEventRow, StoreError> {
    Ok(SafetyEventRow {
        id: r.try_get(0)?,
        time_us: r.try_get(1)?,
        log_id: r.try_get(2)?,
        request_id: r.try_get(3)?,
        node_id: r.try_get(4)?,
        guardrail: r.try_get(5)?,
        guardrail_type: r.try_get(6)?,
        stage: r.try_get(7)?,
        action: r.try_get(8)?,
        category: r.try_get(9)?,
        severity: r.try_get(10)?,
        confidence: r.try_get(11)?,
        score: r.try_get(12)?,
        entities: r.try_get(13)?,
        spans: r.try_get(14)?,
        fingerprint: r.try_get(15)?,
        organization: r.try_get(16)?,
        team: r.try_get(17)?,
        project: r.try_get(18)?,
        key: r.try_get(19)?,
        end_user: r.try_get(20)?,
        model: r.try_get(21)?,
        provider: r.try_get(22)?,
        latency_ms: r.try_get(23)?,
        review_state: r.try_get(24)?,
        reviewed_by: r.try_get(25)?,
        reviewed_us: r.try_get(26)?,
        note: r.try_get(27)?,
    })
}

pub(crate) fn hour_sqlite(r: &rusqlite::Row<'_>) -> rusqlite::Result<GuardrailHour> {
    Ok(GuardrailHour {
        guardrail: r.get(0)?,
        hour_us: r.get(1)?,
        node_id: r.get(2)?,
        evaluations: r.get(3)?,
        hits: r.get(4)?,
        blocks: r.get(5)?,
        redactions: r.get(6)?,
        flags: r.get(7)?,
        errors: r.get(8)?,
        latency: r.get(9)?,
    })
}

pub(crate) fn hour_pg(r: &tokio_postgres::Row) -> Result<GuardrailHour, StoreError> {
    Ok(GuardrailHour {
        guardrail: r.try_get(0)?,
        hour_us: r.try_get(1)?,
        node_id: r.try_get(2)?,
        evaluations: r.try_get(3)?,
        hits: r.try_get(4)?,
        blocks: r.try_get(5)?,
        redactions: r.try_get(6)?,
        flags: r.try_get(7)?,
        errors: r.try_get(8)?,
        latency: r.try_get(9)?,
    })
}

/// The `WHERE` clause of a filter.
fn filter(q: &mut Sql, f: &SafetyFilter) {
    q.push(" WHERE 1 = 1");
    if let Some(from) = f.from_us {
        let m = q.bind(Arg::Int(from));
        q.push(&format!(" AND time_us >= {m}"));
    }
    if let Some(to) = f.to_us {
        let m = q.bind(Arg::Int(to));
        q.push(&format!(" AND time_us < {m}"));
    }
    for (column, values) in &f.any_of {
        let args: Vec<Arg> = values.iter().cloned().map(Arg::Text).collect();
        if args.is_empty() {
            continue;
        }
        q.any_of(column.sql(), &args);
    }
    if let Some(text) = f.q.as_deref().filter(|t| !t.is_empty()) {
        let pattern = format!("%{}%", like_escape(&text.to_lowercase()));
        let a = q.bind(Arg::Text(pattern.clone()));
        let b = q.bind(Arg::Text(pattern));
        q.push(&format!(
            " AND (lower(id) LIKE {a} ESCAPE '\\' OR lower(request_id) LIKE {b} ESCAPE '\\')"
        ));
    }
    if let Some(scopes) = &f.scopes {
        scope_clause(q, scopes, "organization", "team", "project");
    }
}

/// The newest events first, after `(time_us, id)`.
fn list_sql(pg: bool, f: &SafetyFilter, after: Option<&(i64, String)>, limit: usize) -> Sql {
    let mut q = Sql::new(pg, &format!("SELECT {COLUMNS} FROM safety_events"));
    filter(&mut q, f);
    if let Some((t, id)) = after {
        let a = q.bind(Arg::Int(*t));
        let b = q.bind(Arg::Text(id.clone()));
        q.push(&format!(" AND (time_us, id) < ({a}, {b})"));
    }
    let l = q.bind(Arg::Int(i64::try_from(limit).unwrap_or(i64::MAX)));
    q.push(&format!(" ORDER BY time_us DESC, id DESC LIMIT {l}"));
    q
}

fn count_sql(pg: bool, f: &SafetyFilter, cap: u64) -> Sql {
    let mut q = Sql::new(pg, "SELECT count(*) FROM (SELECT 1 FROM safety_events");
    filter(&mut q, f);
    let l = q.bind(Arg::Int(i64::try_from(cap.saturating_add(1)).unwrap_or(i64::MAX)));
    q.push(&format!(" LIMIT {l}) AS capped"));
    q
}

fn delete_sql(pg: bool, f: &SafetyFilter) -> Sql {
    let mut q = Sql::new(pg, "DELETE FROM safety_events");
    filter(&mut q, f);
    q
}

fn sqlite_values(args: &[Arg]) -> Vec<rusqlite::types::Value> {
    args.iter().map(Arg::sqlite).collect()
}

fn sqlite_list(c: &Connection, q: &Sql) -> Result<Vec<SafetyEventRow>, StoreError> {
    let mut stmt = c.prepare(&q.text)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(sqlite_values(&q.args)), sqlite_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn sqlite_insert(c: &mut Connection, rows: &[SafetyEventRow]) -> Result<(), StoreError> {
    let marks: Vec<String> = (1..=COLUMN_COUNT).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "INSERT OR IGNORE INTO safety_events ({COLUMNS}) VALUES ({})",
        marks.join(", ")
    );
    let tx = c.transaction()?;
    {
        let mut stmt = tx.prepare(&sql)?;
        for r in rows {
            let values: Vec<rusqlite::types::Value> = args(r)
                .into_iter()
                .map(|a| a.map_or(rusqlite::types::Value::Null, |a| a.sqlite()))
                .collect();
            stmt.execute(rusqlite::params_from_iter(values))?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn sqlite_merge_hours(
    c: &mut Connection,
    rows: &[GuardrailHour],
    merge: SketchMerge,
) -> Result<(), StoreError> {
    let tx = c.transaction()?;
    {
        let mut find = tx.prepare(&lite(HOUR_FIND))?;
        let mut write = tx.prepare(&lite(HOUR_UPSERT))?;
        for new in rows {
            let stored = find
                .query_row(
                    rusqlite::params![new.guardrail, new.hour_us, new.node_id],
                    hour_sqlite,
                )
                .optional()?;
            let row = match stored {
                Some(mut s) => {
                    s.add(new, merge);
                    s
                }
                None => new.clone(),
            };
            write.execute(rusqlite::params![
                row.guardrail,
                row.hour_us,
                row.node_id,
                row.evaluations,
                row.hits,
                row.blocks,
                row.redactions,
                row.flags,
                row.errors,
                row.latency
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

impl Store {
    /// Stores events; an event already stored is kept.
    pub async fn insert_safety_events(&self, rows: Vec<SafetyEventRow>) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        match self {
            Store::Sqlite(s) => s.logs().call(move |c| sqlite_insert(c, &rows)).await,
            Store::Postgres(p) => p.insert_safety_rows(rows).await,
        }
    }

    /// At most `limit` events matching `f`, newest first, after the position `after`.
    pub async fn list_safety_events(
        &self,
        f: &SafetyFilter,
        after: Option<(i64, String)>,
        limit: usize,
    ) -> Result<Vec<SafetyEventRow>, StoreError> {
        match self {
            Store::Sqlite(s) => {
                let q = list_sql(false, f, after.as_ref(), limit);
                s.logs().call(move |c| sqlite_list(c, &q)).await
            }
            Store::Postgres(p) => {
                let q = list_sql(true, f, after.as_ref(), limit);
                p.safety_rows(&q.text, &q.args).await
            }
        }
    }

    /// How many events match, counting at most `cap + 1`.
    pub async fn count_safety_events(&self, f: &SafetyFilter, cap: u64) -> Result<u64, StoreError> {
        let n: i64 = match self {
            Store::Sqlite(s) => {
                let q = count_sql(false, f, cap);
                s.logs()
                    .call(move |c| {
                        Ok(
                            c.query_row(&q.text, rusqlite::params_from_iter(sqlite_values(&q.args)), |r| {
                                r.get(0)
                            })?,
                        )
                    })
                    .await?
            }
            Store::Postgres(p) => {
                let q = count_sql(true, f, cap);
                p.safety_count(&q.text, &q.args).await?
            }
        };
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// One event.
    pub async fn safety_event(&self, id: &str) -> Result<Option<SafetyEventRow>, StoreError> {
        let f = SafetyFilter {
            any_of: vec![(SafetyColumn::Id, vec![id.to_owned()])],
            ..SafetyFilter::default()
        };
        Ok(self.list_safety_events(&f, None, 1).await?.into_iter().next())
    }

    /// Sets an event's review label; `false` when no event has that id.
    pub async fn review_safety_event(
        &self,
        id: &str,
        state: &str,
        by: &str,
        at_us: i64,
        note: Option<&str>,
    ) -> Result<bool, StoreError> {
        const SQL: &str = "UPDATE safety_events SET review_state = $2, reviewed_by = $3, reviewed_us = $4,
                           note = $5 WHERE id = $1";
        let (id, state, by, note) = (
            id.to_owned(),
            state.to_owned(),
            by.to_owned(),
            note.map(str::to_owned),
        );
        let n = match self {
            Store::Sqlite(s) => {
                s.logs()
                    .call(move |c| Ok(c.execute(&lite(SQL), rusqlite::params![id, state, by, at_us, note])?))
                    .await?
            }
            Store::Postgres(p) => {
                let n = p.execute(SQL, &[&id, &state, &by, &at_us, &note]).await?;
                usize::try_from(n).unwrap_or(usize::MAX)
            }
        };
        Ok(n > 0)
    }

    /// Per guardrail, the false-positive labels and all labels of events since `since_us`.
    pub async fn review_counts(&self, since_us: i64) -> Result<Vec<(String, i64, i64)>, StoreError> {
        const SQL: &str = "SELECT guardrail,
                  sum(CASE WHEN review_state = 'false_positive' THEN 1 ELSE 0 END),
                  count(*)
             FROM safety_events WHERE time_us >= $1 AND review_state <> 'open' GROUP BY guardrail";
        match self {
            Store::Sqlite(s) => {
                s.logs()
                    .call(move |c| {
                        let mut stmt = c.prepare(&lite(SQL))?;
                        let rows = stmt.query_map([since_us], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                        Ok(rows.collect::<Result<Vec<_>, _>>()?)
                    })
                    .await
            }
            Store::Postgres(p) => p
                .rows(SQL, &[&since_us])
                .await?
                .iter()
                .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
                .collect(),
        }
    }

    /// Adds a node's counts to its stored hours.
    pub async fn merge_guardrail_hours(
        &self,
        rows: Vec<GuardrailHour>,
        merge: SketchMerge,
    ) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        match self {
            Store::Sqlite(s) => s.logs().call(move |c| sqlite_merge_hours(c, &rows, merge)).await,
            Store::Postgres(p) => p.merge_hour_rows(rows, merge).await,
        }
    }

    /// Every node's hours from `from_hour_us` on.
    pub async fn guardrail_hours(&self, from_hour_us: i64) -> Result<Vec<GuardrailHour>, StoreError> {
        let sql = format!("SELECT {HOUR_COLUMNS} FROM guardrail_hours WHERE hour_us >= $1");
        match self {
            Store::Sqlite(s) => {
                s.logs()
                    .call(move |c| {
                        let mut stmt = c.prepare(&lite(&sql))?;
                        let rows = stmt.query_map([from_hour_us], hour_sqlite)?;
                        Ok(rows.collect::<Result<Vec<_>, _>>()?)
                    })
                    .await
            }
            Store::Postgres(p) => p
                .rows(&sql, &[&from_hour_us])
                .await?
                .iter()
                .map(hour_pg)
                .collect(),
        }
    }

    /// Deletes events and hours older than `before_us`, or the events `f` matches when given.
    pub async fn expire_safety(&self, before_us: i64, f: Option<&SafetyFilter>) -> Result<u64, StoreError> {
        let pg = matches!(self, Store::Postgres(_));
        let q = match f {
            Some(f) => delete_sql(pg, f),
            None => {
                let mut q = Sql::new(pg, "DELETE FROM safety_events WHERE time_us < ");
                let m = q.bind(Arg::Int(before_us));
                q.push(&m);
                q
            }
        };
        let hours = f.is_none();
        match self {
            Store::Sqlite(s) => {
                s.logs()
                    .call(move |c| {
                        let n = c.execute(&q.text, rusqlite::params_from_iter(sqlite_values(&q.args)))?;
                        if hours {
                            c.execute("DELETE FROM guardrail_hours WHERE hour_us < ?1", [before_us])?;
                        }
                        Ok(n as u64)
                    })
                    .await
            }
            Store::Postgres(p) => {
                let n = p.safety_execute(&q.text, &q.args).await?;
                if hours {
                    p.execute("DELETE FROM guardrail_hours WHERE hour_us < $1", &[&before_us])
                        .await?;
                }
                Ok(n)
            }
        }
    }

    /// Removes every event and hour (test-mode reset only).
    pub async fn clear_safety(&self) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.logs()
                    .call(|c| {
                        c.execute_batch("DELETE FROM safety_events; DELETE FROM guardrail_hours;")?;
                        Ok(())
                    })
                    .await
            }
            Store::Postgres(p) => {
                p.execute("DELETE FROM safety_events", &[]).await?;
                p.execute("DELETE FROM guardrail_hours", &[]).await.map(|_| ())
            }
        }
    }
}

#[cfg(test)]
mod tests;

//! What the console adds to budgets at run time: overage grants, overage requests, the budget event
//! feed and per-node tallies of refused and downgraded requests. Budgets are named by their
//! identity (the counter id without its window, hex), never by partition values. An overage
//! request is unique per budget and period, and an event with a `once` key is written once, so
//! nodes noticing the same thing at the same time record it once.

use rusqlite::types::Value as LiteValue;

use crate::{Store, StoreError};

/// One overage grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantRow {
    pub id: String,
    pub identity: String,
    pub level: String,
    pub owner: String,
    pub name: String,
    /// In the budget's units (nUSD or tokens), as a decimal integer.
    pub amount: String,
    pub created_us: i64,
    pub expires_us: i64,
    pub reason: String,
    pub granted_by: String,
    pub revoked_us: Option<i64>,
    /// The overage request it approved, if any.
    pub request: Option<String>,
}

/// One overage request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverageRow {
    pub id: String,
    pub identity: String,
    pub period_start_us: i64,
    pub period_end_us: Option<i64>,
    pub level: String,
    pub owner: String,
    pub name: String,
    pub opened_us: i64,
    /// Spent and amount when it was opened, in the budget's units.
    pub spent: String,
    pub amount: String,
    /// `open`, `approved`, `denied` or `expired`.
    pub state: String,
    pub version: i64,
    pub decided_by: Option<String>,
    pub decided_us: Option<i64>,
    pub reason: Option<String>,
    pub grant: Option<String>,
}

/// One event of the budget feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetEventRow {
    pub id: String,
    pub time_us: i64,
    pub kind: String,
    pub level: String,
    pub owner: String,
    pub name: String,
    pub actor: Option<String>,
    /// JSON.
    pub detail: String,
    /// The budget identity, when the event is about one counter.
    pub identity: Option<String>,
}

/// What an event listing selects, newest first.
#[derive(Clone, Debug, Default)]
pub struct BudgetEventFilter {
    pub level: Option<String>,
    pub owner: Option<String>,
    pub name: Option<String>,
    pub kinds: Vec<String>,
    pub from_us: Option<i64>,
    pub to_us: Option<i64>,
    /// Only events older than this `(time, id)` position.
    pub before: Option<(i64, String)>,
    pub limit: u64,
}

/// Requests one node counted for a budget in a period.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TallyRow {
    pub identity: String,
    pub period_start_us: i64,
    pub node: String,
    pub refused: i64,
    pub downgraded: i64,
}

pub(crate) const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS budget_grants (
        id TEXT PRIMARY KEY,
        identity TEXT NOT NULL,
        level TEXT NOT NULL,
        owner TEXT NOT NULL,
        name TEXT NOT NULL,
        amount TEXT NOT NULL,
        created_us INTEGER NOT NULL,
        expires_us INTEGER NOT NULL,
        reason TEXT NOT NULL,
        granted_by TEXT NOT NULL,
        revoked_us INTEGER,
        request TEXT)",
    "CREATE INDEX IF NOT EXISTS budget_grants_identity ON budget_grants (identity, expires_us)",
    "CREATE TABLE IF NOT EXISTS overage_requests (
        id TEXT PRIMARY KEY,
        identity TEXT NOT NULL,
        period_start_us INTEGER NOT NULL,
        period_end_us INTEGER,
        level TEXT NOT NULL,
        owner TEXT NOT NULL,
        name TEXT NOT NULL,
        opened_us INTEGER NOT NULL,
        spent TEXT NOT NULL,
        amount TEXT NOT NULL,
        state TEXT NOT NULL,
        version INTEGER NOT NULL,
        decided_by TEXT,
        decided_us INTEGER,
        reason TEXT,
        grant_id TEXT,
        UNIQUE (identity, period_start_us))",
    "CREATE TABLE IF NOT EXISTS budget_events (
        id TEXT PRIMARY KEY,
        time_us INTEGER NOT NULL,
        kind TEXT NOT NULL,
        level TEXT NOT NULL,
        owner TEXT NOT NULL,
        name TEXT NOT NULL,
        actor TEXT,
        detail TEXT NOT NULL,
        identity TEXT,
        once_key TEXT UNIQUE)",
    "CREATE INDEX IF NOT EXISTS budget_events_time ON budget_events (time_us, id)",
    "CREATE TABLE IF NOT EXISTS budget_tallies (
        identity TEXT NOT NULL,
        period_start_us INTEGER NOT NULL,
        node TEXT NOT NULL,
        refused INTEGER NOT NULL,
        downgraded INTEGER NOT NULL,
        PRIMARY KEY (identity, period_start_us, node))",
];

pub(crate) const PG_MIGRATION: &str = "CREATE TABLE IF NOT EXISTS budget_grants (
        id text PRIMARY KEY,
        identity text NOT NULL,
        level text NOT NULL,
        owner text NOT NULL,
        name text NOT NULL,
        amount text NOT NULL,
        created_us bigint NOT NULL,
        expires_us bigint NOT NULL,
        reason text NOT NULL,
        granted_by text NOT NULL,
        revoked_us bigint,
        request text);
     CREATE INDEX IF NOT EXISTS budget_grants_identity ON budget_grants (identity, expires_us);
     CREATE TABLE IF NOT EXISTS overage_requests (
        id text PRIMARY KEY,
        identity text NOT NULL,
        period_start_us bigint NOT NULL,
        period_end_us bigint,
        level text NOT NULL,
        owner text NOT NULL,
        name text NOT NULL,
        opened_us bigint NOT NULL,
        spent text NOT NULL,
        amount text NOT NULL,
        state text NOT NULL,
        version bigint NOT NULL,
        decided_by text,
        decided_us bigint,
        reason text,
        grant_id text,
        UNIQUE (identity, period_start_us));
     CREATE TABLE IF NOT EXISTS budget_events (
        id text PRIMARY KEY,
        time_us bigint NOT NULL,
        kind text NOT NULL,
        level text NOT NULL,
        owner text NOT NULL,
        name text NOT NULL,
        actor text,
        detail text NOT NULL,
        identity text,
        once_key text UNIQUE);
     CREATE INDEX IF NOT EXISTS budget_events_time ON budget_events (time_us, id);
     CREATE TABLE IF NOT EXISTS budget_tallies (
        identity text NOT NULL,
        period_start_us bigint NOT NULL,
        node text NOT NULL,
        refused bigint NOT NULL,
        downgraded bigint NOT NULL,
        PRIMARY KEY (identity, period_start_us, node));";

/// A statement parameter, typed for both backends.
#[derive(Clone, Debug)]
pub(crate) enum Arg {
    Text(Option<String>),
    Int(Option<i64>),
}

impl From<&str> for Arg {
    fn from(s: &str) -> Arg {
        Arg::Text(Some(s.to_owned()))
    }
}

impl From<String> for Arg {
    fn from(s: String) -> Arg {
        Arg::Text(Some(s))
    }
}

impl From<Option<String>> for Arg {
    fn from(s: Option<String>) -> Arg {
        Arg::Text(s)
    }
}

impl From<i64> for Arg {
    fn from(v: i64) -> Arg {
        Arg::Int(Some(v))
    }
}

impl From<Option<i64>> for Arg {
    fn from(v: Option<i64>) -> Arg {
        Arg::Int(v)
    }
}

/// A column of a result row.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Col {
    Text,
    Int,
}

/// A value read from a result row.
#[derive(Clone, Debug)]
pub(crate) enum Cell {
    Text(Option<String>),
    Int(Option<i64>),
}

/// Reads the cells of a row in order.
pub(crate) struct Cells(std::vec::IntoIter<Cell>);

impl Cells {
    pub(crate) fn text(&mut self) -> String {
        self.opt_text().unwrap_or_default()
    }

    pub(crate) fn opt_text(&mut self) -> Option<String> {
        match self.0.next() {
            Some(Cell::Text(t)) => t,
            _ => None,
        }
    }

    pub(crate) fn int(&mut self) -> i64 {
        self.opt_int().unwrap_or_default()
    }

    pub(crate) fn opt_int(&mut self) -> Option<i64> {
        match self.0.next() {
            Some(Cell::Int(v)) => v,
            _ => None,
        }
    }
}

fn lite(sql: &str) -> String {
    sql.replace('$', "?")
}

fn lite_args(args: Vec<Arg>) -> Vec<LiteValue> {
    args.into_iter()
        .map(|a| match a {
            Arg::Text(Some(t)) => LiteValue::Text(t),
            Arg::Int(Some(v)) => LiteValue::Integer(v),
            Arg::Text(None) | Arg::Int(None) => LiteValue::Null,
        })
        .collect()
}

type PgArg = Box<dyn tokio_postgres::types::ToSql + Sync + Send>;

fn pg_args(args: Vec<Arg>) -> Vec<PgArg> {
    args.into_iter()
        .map(|a| -> PgArg {
            match a {
                Arg::Text(t) => Box::new(t),
                Arg::Int(v) => Box::new(v),
            }
        })
        .collect()
}

impl Store {
    /// Runs one statement written with `$n` parameters; the rows it changed.
    pub(crate) async fn run_statement(&self, sql: &str, args: Vec<Arg>) -> Result<u64, StoreError> {
        match self {
            Store::Sqlite(s) => {
                let sql = lite(sql);
                s.call(move |c| {
                    let n = c.execute(&sql, rusqlite::params_from_iter(lite_args(args)))?;
                    Ok(u64::try_from(n).unwrap_or(0))
                })
                .await
            }
            Store::Postgres(p) => {
                let boxed = pg_args(args);
                let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = boxed
                    .iter()
                    .map(|b| b.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
                    .collect();
                p.execute(sql, &refs).await
            }
        }
    }

    /// The rows of one query written with `$n` parameters, read as `cols`.
    pub(crate) async fn query_cells(
        &self,
        sql: &str,
        args: Vec<Arg>,
        cols: &'static [Col],
    ) -> Result<Vec<Cells>, StoreError> {
        match self {
            Store::Sqlite(s) => {
                let sql = lite(sql);
                s.call(move |c| {
                    let mut stmt = c.prepare(&sql)?;
                    let rows = stmt.query_map(rusqlite::params_from_iter(lite_args(args)), |r| {
                        let mut out = Vec::with_capacity(cols.len());
                        for (i, col) in cols.iter().enumerate() {
                            out.push(match col {
                                Col::Text => Cell::Text(r.get(i)?),
                                Col::Int => Cell::Int(r.get(i)?),
                            });
                        }
                        Ok(Cells(out.into_iter()))
                    })?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
                .await
            }
            Store::Postgres(p) => {
                let boxed = pg_args(args);
                let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = boxed
                    .iter()
                    .map(|b| b.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
                    .collect();
                let rows = p.rows(sql, &refs).await?;
                let mut out = Vec::with_capacity(rows.len());
                for r in &rows {
                    let mut cells = Vec::with_capacity(cols.len());
                    for (i, col) in cols.iter().enumerate() {
                        cells.push(match col {
                            Col::Text => Cell::Text(r.try_get(i)?),
                            Col::Int => Cell::Int(r.try_get(i)?),
                        });
                    }
                    out.push(Cells(cells.into_iter()));
                }
                Ok(out)
            }
        }
    }
}

const GRANT_COLUMNS: &str = "id, identity, level, owner, name, amount, created_us, expires_us, reason, \
                             granted_by, revoked_us, request";
const GRANT_COLS: &[Col] = &[
    Col::Text,
    Col::Text,
    Col::Text,
    Col::Text,
    Col::Text,
    Col::Text,
    Col::Int,
    Col::Int,
    Col::Text,
    Col::Text,
    Col::Int,
    Col::Text,
];

fn grant(mut c: Cells) -> GrantRow {
    GrantRow {
        id: c.text(),
        identity: c.text(),
        level: c.text(),
        owner: c.text(),
        name: c.text(),
        amount: c.text(),
        created_us: c.int(),
        expires_us: c.int(),
        reason: c.text(),
        granted_by: c.text(),
        revoked_us: c.opt_int(),
        request: c.opt_text(),
    }
}

const OVERAGE_COLUMNS: &str = "id, identity, period_start_us, period_end_us, level, owner, name, opened_us, \
                               spent, amount, state, version, decided_by, decided_us, reason, grant_id";
const OVERAGE_COLS: &[Col] = &[
    Col::Text,
    Col::Text,
    Col::Int,
    Col::Int,
    Col::Text,
    Col::Text,
    Col::Text,
    Col::Int,
    Col::Text,
    Col::Text,
    Col::Text,
    Col::Int,
    Col::Text,
    Col::Int,
    Col::Text,
    Col::Text,
];

fn overage(mut c: Cells) -> OverageRow {
    OverageRow {
        id: c.text(),
        identity: c.text(),
        period_start_us: c.int(),
        period_end_us: c.opt_int(),
        level: c.text(),
        owner: c.text(),
        name: c.text(),
        opened_us: c.int(),
        spent: c.text(),
        amount: c.text(),
        state: c.text(),
        version: c.int(),
        decided_by: c.opt_text(),
        decided_us: c.opt_int(),
        reason: c.opt_text(),
        grant: c.opt_text(),
    }
}

const EVENT_COLUMNS: &str = "id, time_us, kind, level, owner, name, actor, detail, identity";
const EVENT_COLS: &[Col] = &[
    Col::Text,
    Col::Int,
    Col::Text,
    Col::Text,
    Col::Text,
    Col::Text,
    Col::Text,
    Col::Text,
    Col::Text,
];

fn event(mut c: Cells) -> BudgetEventRow {
    BudgetEventRow {
        id: c.text(),
        time_us: c.int(),
        kind: c.text(),
        level: c.text(),
        owner: c.text(),
        name: c.text(),
        actor: c.opt_text(),
        detail: c.text(),
        identity: c.opt_text(),
    }
}

impl Store {
    pub async fn insert_grant(&self, g: GrantRow) -> Result<(), StoreError> {
        let sql = format!(
            "INSERT INTO budget_grants ({GRANT_COLUMNS})
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"
        );
        let args = vec![
            g.id.into(),
            g.identity.into(),
            g.level.into(),
            g.owner.into(),
            g.name.into(),
            g.amount.into(),
            g.created_us.into(),
            g.expires_us.into(),
            g.reason.into(),
            g.granted_by.into(),
            g.revoked_us.into(),
            g.request.into(),
        ];
        self.run_statement(&sql, args).await.map(|_| ())
    }

    /// Grants neither revoked nor expired at `now_us`, of one budget or of all.
    pub async fn running_grants(
        &self,
        identity: Option<&str>,
        now_us: i64,
    ) -> Result<Vec<GrantRow>, StoreError> {
        let mut sql =
            format!("SELECT {GRANT_COLUMNS} FROM budget_grants WHERE revoked_us IS NULL AND expires_us > $1");
        let mut args: Vec<Arg> = vec![now_us.into()];
        if let Some(i) = identity {
            sql.push_str(" AND identity = $2");
            args.push(i.into());
        }
        sql.push_str(" ORDER BY created_us, id");
        Ok(self
            .query_cells(&sql, args, GRANT_COLS)
            .await?
            .into_iter()
            .map(grant)
            .collect())
    }

    pub async fn grant(&self, id: &str) -> Result<Option<GrantRow>, StoreError> {
        let sql = format!("SELECT {GRANT_COLUMNS} FROM budget_grants WHERE id = $1");
        Ok(self
            .query_cells(&sql, vec![id.into()], GRANT_COLS)
            .await?
            .into_iter()
            .map(grant)
            .next())
    }

    /// Revokes a running grant; `false` when it is unknown, revoked or expired.
    pub async fn revoke_grant(&self, id: &str, now_us: i64) -> Result<bool, StoreError> {
        let n = self
            .run_statement(
                "UPDATE budget_grants SET revoked_us = $2
                 WHERE id = $1 AND revoked_us IS NULL AND expires_us > $2",
                vec![id.into(), now_us.into()],
            )
            .await?;
        Ok(n > 0)
    }

    /// Opens an overage request; `false` when the budget already has one for that period.
    pub async fn open_overage(&self, o: OverageRow) -> Result<bool, StoreError> {
        let sql = format!(
            "INSERT INTO overage_requests ({OVERAGE_COLUMNS})
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
             ON CONFLICT (identity, period_start_us) DO NOTHING"
        );
        let args = vec![
            o.id.into(),
            o.identity.into(),
            o.period_start_us.into(),
            o.period_end_us.into(),
            o.level.into(),
            o.owner.into(),
            o.name.into(),
            o.opened_us.into(),
            o.spent.into(),
            o.amount.into(),
            o.state.into(),
            o.version.into(),
            o.decided_by.into(),
            o.decided_us.into(),
            o.reason.into(),
            o.grant.into(),
        ];
        Ok(self.run_statement(&sql, args).await? > 0)
    }

    pub async fn overage(&self, id: &str) -> Result<Option<OverageRow>, StoreError> {
        let sql = format!("SELECT {OVERAGE_COLUMNS} FROM overage_requests WHERE id = $1");
        Ok(self
            .query_cells(&sql, vec![id.into()], OVERAGE_COLS)
            .await?
            .into_iter()
            .map(overage)
            .next())
    }

    /// Overage requests in one state or all, newest first, at most `limit`.
    pub async fn overages(&self, state: Option<&str>, limit: u64) -> Result<Vec<OverageRow>, StoreError> {
        let mut sql = format!("SELECT {OVERAGE_COLUMNS} FROM overage_requests");
        let mut args: Vec<Arg> = vec![i64::try_from(limit).unwrap_or(i64::MAX).into()];
        if let Some(s) = state {
            sql.push_str(" WHERE state = $2");
            args.push(s.into());
        }
        sql.push_str(" ORDER BY opened_us DESC, id DESC LIMIT $1");
        Ok(self
            .query_cells(&sql, args, OVERAGE_COLS)
            .await?
            .into_iter()
            .map(overage)
            .collect())
    }

    /// Moves a request at `version` to its decided state; `false` when another writer moved it
    /// first.
    pub async fn decide_overage(&self, o: &OverageRow) -> Result<bool, StoreError> {
        let n = self
            .run_statement(
                "UPDATE overage_requests SET state = $3, decided_by = $4, decided_us = $5, reason = $6,
                    grant_id = $7, version = version + 1
                 WHERE id = $1 AND version = $2",
                vec![
                    o.id.clone().into(),
                    o.version.into(),
                    o.state.clone().into(),
                    o.decided_by.clone().into(),
                    o.decided_us.into(),
                    o.reason.clone().into(),
                    o.grant.clone().into(),
                ],
            )
            .await?;
        Ok(n > 0)
    }

    /// Expires the open requests whose period ended by `now_us`.
    pub async fn expire_overages(&self, now_us: i64) -> Result<u64, StoreError> {
        self.run_statement(
            "UPDATE overage_requests SET state = 'expired', decided_us = period_end_us, version = version + 1
             WHERE state = 'open' AND period_end_us IS NOT NULL AND period_end_us <= $1",
            vec![now_us.into()],
        )
        .await
    }

    /// Records an event; with `once`, only the first event of that key is kept. `false` when it
    /// was already recorded.
    pub async fn insert_budget_event(
        &self,
        e: BudgetEventRow,
        once: Option<String>,
    ) -> Result<bool, StoreError> {
        let sql = format!(
            "INSERT INTO budget_events ({EVENT_COLUMNS}, once_key)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT DO NOTHING"
        );
        let args = vec![
            e.id.into(),
            e.time_us.into(),
            e.kind.into(),
            e.level.into(),
            e.owner.into(),
            e.name.into(),
            e.actor.into(),
            e.detail.into(),
            e.identity.into(),
            once.into(),
        ];
        Ok(self.run_statement(&sql, args).await? > 0)
    }

    /// Events matching `f`, newest first.
    pub async fn budget_events(&self, f: &BudgetEventFilter) -> Result<Vec<BudgetEventRow>, StoreError> {
        let mut sql = format!("SELECT {EVENT_COLUMNS} FROM budget_events WHERE 1 = 1");
        let mut args: Vec<Arg> = vec![i64::try_from(f.limit).unwrap_or(i64::MAX).into()];
        let mut param = |a: Arg| -> String {
            args.push(a);
            format!("${}", args.len())
        };
        for (column, value) in [("level", &f.level), ("owner", &f.owner), ("name", &f.name)] {
            if let Some(v) = value {
                sql.push_str(&format!(" AND {column} = {}", param(v.clone().into())));
            }
        }
        if let Some(t) = f.from_us {
            sql.push_str(&format!(" AND time_us >= {}", param(t.into())));
        }
        if let Some(t) = f.to_us {
            sql.push_str(&format!(" AND time_us < {}", param(t.into())));
        }
        if !f.kinds.is_empty() {
            let parts: Vec<String> = f.kinds.iter().map(|k| param(k.clone().into())).collect();
            sql.push_str(&format!(" AND kind IN ({})", parts.join(", ")));
        }
        if let Some((t, id)) = &f.before {
            let (t, id) = (param((*t).into()), param(id.clone().into()));
            sql.push_str(&format!(" AND (time_us < {t} OR (time_us = {t} AND id < {id}))"));
        }
        sql.push_str(" ORDER BY time_us DESC, id DESC LIMIT $1");
        Ok(self
            .query_cells(&sql, args, EVENT_COLS)
            .await?
            .into_iter()
            .map(event)
            .collect())
    }

    /// Adds each node's counts to its stored tallies.
    pub async fn add_tallies(&self, rows: Vec<TallyRow>) -> Result<(), StoreError> {
        for r in rows {
            self.run_statement(
                "INSERT INTO budget_tallies (identity, period_start_us, node, refused, downgraded)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (identity, period_start_us, node) DO UPDATE SET
                    refused = budget_tallies.refused + excluded.refused,
                    downgraded = budget_tallies.downgraded + excluded.downgraded",
                vec![
                    r.identity.into(),
                    r.period_start_us.into(),
                    r.node.into(),
                    r.refused.into(),
                    r.downgraded.into(),
                ],
            )
            .await?;
        }
        Ok(())
    }

    /// Refused and downgraded requests of a budget in the period starting at `period_start_us`, every
    /// node's.
    pub async fn tally(&self, identity: &str, period_start_us: i64) -> Result<(i64, i64), StoreError> {
        let rows = self
            .query_cells(
                "SELECT CAST(COALESCE(SUM(refused), 0) AS BIGINT), CAST(COALESCE(SUM(downgraded), 0) AS BIGINT)
                 FROM budget_tallies WHERE identity = $1 AND period_start_us = $2",
                vec![identity.into(), period_start_us.into()],
                &[Col::Int, Col::Int],
            )
            .await?;
        Ok(rows.into_iter().next().map_or((0, 0), |mut c| (c.int(), c.int())))
    }

    /// Forgets grants, overage requests, events and tallies (test mode).
    pub async fn clear_budget_console(&self) -> Result<(), StoreError> {
        for table in [
            "budget_grants",
            "overage_requests",
            "budget_events",
            "budget_tallies",
        ] {
            self.run_statement(&format!("DELETE FROM {table}"), Vec::new())
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::pg::{Migrate, PgSettings};

    fn embedded() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "antirion-store-budgets-{:x}",
            antirion_core::ResourceUid::random().0
        ));
        Store::open_embedded(&dir).unwrap()
    }

    /// A store on a fresh database of `ANTIRION_TEST_POSTGRES_URL`, when it is set.
    async fn postgres() -> Option<Store> {
        let base = std::env::var("ANTIRION_TEST_POSTGRES_URL").ok()?;
        let name = format!("t_{:x}", antirion_core::ResourceUid::random().0);
        let (client, conn) = tokio_postgres::connect(&base, tokio_postgres::NoTls).await.ok()?;
        let mut set = tokio::task::JoinSet::new();
        set.spawn(async move {
            let _ = conn.await;
        });
        client
            .batch_execute(&format!("CREATE DATABASE {name}"))
            .await
            .ok()?;
        let (prefix, _) = base.rsplit_once('/')?;
        let settings = PgSettings {
            url: format!("{prefix}/{name}"),
            pool_size: 2,
            connect_timeout: Duration::from_secs(5),
            statement_timeout: Duration::from_secs(5),
            migrate: Migrate::Auto,
            migrate_timeout: Duration::from_secs(30),
            binary_version: "0.1.0".into(),
            node_id: "n".into(),
        };
        Some(Store::open_postgres(&settings).await.unwrap())
    }

    fn grant_row(id: &str, expires_us: i64) -> GrantRow {
        GrantRow {
            id: id.into(),
            identity: "b1".into(),
            level: "team".into(),
            owner: "t".into(),
            name: "monthly".into(),
            amount: "100000000000".into(),
            created_us: 10,
            expires_us,
            reason: "launch".into(),
            granted_by: "ada".into(),
            revoked_us: None,
            request: None,
        }
    }

    fn request_row(id: &str, period_start_us: i64) -> OverageRow {
        OverageRow {
            id: id.into(),
            identity: "b1".into(),
            period_start_us,
            period_end_us: Some(period_start_us + 100),
            level: "team".into(),
            owner: "t".into(),
            name: "monthly".into(),
            opened_us: period_start_us + 1,
            spent: "5".into(),
            amount: "5".into(),
            state: "open".into(),
            version: 0,
            decided_by: None,
            decided_us: None,
            reason: None,
            grant: None,
        }
    }

    fn event_row(id: &str, time_us: i64, kind: &str) -> BudgetEventRow {
        BudgetEventRow {
            id: id.into(),
            time_us,
            kind: kind.into(),
            level: "team".into(),
            owner: "t".into(),
            name: "monthly".into(),
            actor: None,
            detail: "{}".into(),
            identity: Some("b1".into()),
        }
    }

    async fn grants_behave(s: &Store) {
        s.insert_grant(grant_row("g1", 50)).await.unwrap();
        s.insert_grant(grant_row("g2", 500)).await.unwrap();
        let running = s.running_grants(None, 60).await.unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, "g2");
        assert_eq!(s.running_grants(Some("other"), 0).await.unwrap(), Vec::new());
        assert!(s.revoke_grant("g2", 70).await.unwrap());
        assert!(!s.revoke_grant("g2", 71).await.unwrap());
        assert!(
            !s.revoke_grant("g1", 71).await.unwrap(),
            "an expired grant is not revoked"
        );
        assert_eq!(s.grant("g2").await.unwrap().unwrap().revoked_us, Some(70));
        assert!(s.running_grants(None, 60).await.unwrap().is_empty());
    }

    async fn requests_behave(s: &Store) {
        assert!(s.open_overage(request_row("r1", 1_000)).await.unwrap());
        assert!(
            !s.open_overage(request_row("r2", 1_000)).await.unwrap(),
            "one per period"
        );
        assert!(s.open_overage(request_row("r3", 2_000)).await.unwrap());
        let mut r = s.overage("r1").await.unwrap().unwrap();
        r.state = "approved".into();
        r.decided_by = Some("ada".into());
        r.grant = Some("g9".into());
        assert!(s.decide_overage(&r).await.unwrap());
        assert!(!s.decide_overage(&r).await.unwrap(), "a stale version loses");
        assert_eq!(s.overage("r1").await.unwrap().unwrap().version, 1);
        assert_eq!(s.expire_overages(2_100).await.unwrap(), 1);
        let all = s.overages(None, 10).await.unwrap();
        assert_eq!(
            all.iter()
                .map(|o| (o.id.as_str(), o.state.as_str()))
                .collect::<Vec<_>>(),
            vec![("r3", "expired"), ("r1", "approved")]
        );
        assert_eq!(s.overages(Some("approved"), 10).await.unwrap().len(), 1);
    }

    async fn events_behave(s: &Store) {
        let once = Some("exhausted|b1|1000".to_owned());
        assert!(
            s.insert_budget_event(event_row("e1", 10, "exhausted"), once.clone())
                .await
                .unwrap()
        );
        assert!(
            !s.insert_budget_event(event_row("e2", 11, "exhausted"), once)
                .await
                .unwrap()
        );
        assert!(
            s.insert_budget_event(event_row("e3", 20, "adjusted"), None)
                .await
                .unwrap()
        );
        assert!(
            s.insert_budget_event(event_row("e4", 20, "reset"), None)
                .await
                .unwrap()
        );
        let page = |before: Option<(i64, String)>, kinds: Vec<String>| {
            let f = BudgetEventFilter {
                kinds,
                before,
                limit: 2,
                ..BudgetEventFilter::default()
            };
            async move { s.budget_events(&f).await.unwrap() }
        };
        let first = page(None, Vec::new()).await;
        assert_eq!(
            first.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["e4", "e3"]
        );
        let rest = page(Some((20, "e3".into())), Vec::new()).await;
        assert_eq!(rest.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["e1"]);
        let kinds = page(None, vec!["exhausted".into(), "reset".into()]).await;
        assert_eq!(kinds.len(), 2);
        let ranged = s
            .budget_events(&BudgetEventFilter {
                from_us: Some(15),
                to_us: Some(21),
                owner: Some("t".into()),
                limit: 10,
                ..BudgetEventFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(ranged.len(), 2);
    }

    async fn tallies_behave(s: &Store) {
        let row = |node: &str, refused, downgraded| TallyRow {
            identity: "b1".into(),
            period_start_us: 1_000,
            node: node.into(),
            refused,
            downgraded,
        };
        s.add_tallies(vec![row("a", 2, 1), row("b", 3, 0)]).await.unwrap();
        s.add_tallies(vec![row("a", 1, 1)]).await.unwrap();
        assert_eq!(s.tally("b1", 1_000).await.unwrap(), (6, 2));
        assert_eq!(s.tally("b1", 2_000).await.unwrap(), (0, 0));
    }

    async fn generations_behave(s: &Store) {
        for g in 1..=4u64 {
            s.put_generation_document(g, format!("{{\"g\":{g}}}"), "{}".into(), 2)
                .await
                .unwrap();
        }
        assert!(s.generation_document(2).await.unwrap().is_none());
        assert_eq!(s.generation_document(4).await.unwrap().unwrap().0, "{\"g\":4}");
    }

    async fn all_behave(s: &Store) {
        grants_behave(s).await;
        requests_behave(s).await;
        events_behave(s).await;
        tallies_behave(s).await;
        generations_behave(s).await;
        s.clear_budget_console().await.unwrap();
        assert!(s.overages(None, 10).await.unwrap().is_empty());
        assert_eq!(s.tally("b1", 1_000).await.unwrap(), (0, 0));
    }

    #[tokio::test]
    async fn budget_console_state_on_the_embedded_store() {
        all_behave(&embedded()).await;
    }

    #[tokio::test]
    async fn budget_console_state_on_postgres() {
        let Some(store) = postgres().await else {
            return;
        };
        all_behave(&store).await;
    }
}

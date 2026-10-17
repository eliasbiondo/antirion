//! Log queries as SQL, written once for every backend: placeholders are `?` on SQLite and
//! ClickHouse and `$n` on PostgreSQL, and every value is a bound argument.

use rusqlite::Connection;

use super::{ListedLog, LogFilter, LogOrder, LogRow, ScopeRef, SortKey, SortPos, StoredLog};
use crate::StoreError;

/// A bound value.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Int(i64),
    Real(f64),
    Text(String),
}

impl Arg {
    pub(crate) fn sqlite(&self) -> rusqlite::types::Value {
        match self {
            Arg::Int(i) => rusqlite::types::Value::Integer(*i),
            Arg::Real(r) => rusqlite::types::Value::Real(*r),
            Arg::Text(t) => rusqlite::types::Value::Text(t.clone()),
        }
    }

    pub(crate) fn pg(&self) -> Box<dyn tokio_postgres::types::ToSql + Sync + Send> {
        match self {
            Arg::Int(i) => Box::new(*i),
            Arg::Real(r) => Box::new(*r),
            Arg::Text(t) => Box::new(t.clone()),
        }
    }
}

/// The SQL dialect a statement is written in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Dialect {
    Sqlite,
    Postgres,
    /// `LIKE` escapes with a backslash and has no `ESCAPE` clause.
    ClickHouse,
}

/// A statement being written: its text and its arguments.
#[derive(Debug)]
pub(crate) struct Sql {
    pub(crate) text: String,
    pub(crate) args: Vec<Arg>,
    pg: bool,
    dialect: Dialect,
}

impl Sql {
    pub(crate) fn new(pg: bool, head: &str) -> Sql {
        let dialect = if pg { Dialect::Postgres } else { Dialect::Sqlite };
        Sql::in_dialect(dialect, head)
    }

    pub(crate) fn in_dialect(dialect: Dialect, head: &str) -> Sql {
        Sql {
            text: head.to_owned(),
            args: Vec::new(),
            pg: dialect == Dialect::Postgres,
            dialect,
        }
    }

    /// The escape clause of a `LIKE` whose pattern escapes with a backslash.
    fn escape_clause(&self) -> &'static str {
        if self.dialect == Dialect::ClickHouse {
            ""
        } else {
            " ESCAPE '\\'"
        }
    }

    /// Binds `a` and returns its placeholder.
    pub(crate) fn bind(&mut self, a: Arg) -> String {
        self.args.push(a);
        if self.pg {
            format!("${}", self.args.len())
        } else {
            "?".to_owned()
        }
    }

    pub(crate) fn push(&mut self, s: &str) {
        self.text.push_str(s);
    }

    /// `AND column IN (…)`; nothing for no values.
    pub(crate) fn any_of(&mut self, column: &str, values: &[Arg]) {
        if values.is_empty() {
            return;
        }
        let marks: Vec<String> = values.iter().map(|v| self.bind(v.clone())).collect();
        self.push(&format!(" AND {column} IN ({})", marks.join(", ")));
    }

    /// `AND (column LIKE '%␟v␟%' OR …)` over a [`super::set`] column.
    fn any_in_set(&mut self, column: &str, values: &[String]) {
        if values.is_empty() {
            return;
        }
        let parts: Vec<String> = values
            .iter()
            .map(|v| {
                let pattern = format!("%\u{1f}{}\u{1f}%", like_escape(v));
                let mark = self.bind(Arg::Text(pattern));
                format!("{column} LIKE {mark}{}", self.escape_clause())
            })
            .collect();
        self.push(&format!(" AND ({})", parts.join(" OR ")));
    }

    fn case_insensitive_like(&self) -> &'static str {
        if self.dialect == Dialect::Sqlite {
            "LIKE"
        } else {
            "ILIKE"
        }
    }
}

pub(crate) fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The columns of `request_logs`, in [`row_args`] order.
pub(crate) const COLUMNS: &str = "id, started_us, node_id, request_id, organization, team, project, key_name, \
     user_name, model, requested_model, provider, deployment, route, route_node, status_code, code, response_path, \
     operation, end_user, session_id, environment, tags, guardrails, verdicts, categories, flagged, stream, \
     cache_status, latency_ms, ttft_ms, cost_nusd, input_tokens, output_tokens, client_ip, record, content, \
     prompt";

pub(crate) const COLUMN_COUNT: usize = 38;

/// A row's values in [`COLUMNS`] order: `None` is SQL NULL.
pub(crate) fn row_args(r: &LogRow) -> Vec<Option<Arg>> {
    let t = |v: &Option<String>| v.clone().map(Arg::Text);
    vec![
        Some(Arg::Text(r.id.clone())),
        Some(Arg::Int(r.started_us)),
        Some(Arg::Text(r.node_id.clone())),
        Some(Arg::Text(r.request_id.clone())),
        t(&r.organization),
        t(&r.team),
        t(&r.project),
        t(&r.key),
        t(&r.user),
        t(&r.model),
        t(&r.requested_model),
        t(&r.provider),
        t(&r.deployment),
        t(&r.route),
        t(&r.route_node),
        Some(Arg::Int(r.status_code)),
        t(&r.code),
        Some(Arg::Text(r.response_path.clone())),
        Some(Arg::Text(r.operation.clone())),
        t(&r.end_user),
        t(&r.session_id),
        t(&r.environment),
        Some(Arg::Text(r.tags.clone())),
        Some(Arg::Text(r.guardrails.clone())),
        Some(Arg::Text(r.verdicts.clone())),
        Some(Arg::Text(r.categories.clone())),
        Some(Arg::Int(i64::from(r.flagged))),
        Some(Arg::Int(i64::from(r.stream))),
        t(&r.cache),
        Some(Arg::Real(r.latency_ms)),
        Some(Arg::Real(r.ttft_ms)),
        Some(Arg::Int(r.cost_nusd)),
        Some(Arg::Int(r.input_tokens)),
        Some(Arg::Int(r.output_tokens)),
        t(&r.client_ip),
        Some(Arg::Text(r.record.clone())),
        t(&r.content),
        t(&r.prompt),
    ]
}

/// The `WHERE` clause of a filter.
pub(crate) fn filter(q: &mut Sql, f: &LogFilter) {
    q.push(" WHERE 1 = 1");
    if let Some(from) = f.from_us {
        let m = q.bind(Arg::Int(from));
        q.push(&format!(" AND started_us >= {m}"));
    }
    if let Some(to) = f.to_us {
        let m = q.bind(Arg::Int(to));
        q.push(&format!(" AND started_us < {m}"));
    }
    for (column, values) in &f.any_of {
        q.any_of(column.sql(), values);
    }
    if !f.status_classes.is_empty() {
        let parts: Vec<String> = f
            .status_classes
            .iter()
            .map(|c| {
                let lo = q.bind(Arg::Int(i64::from(*c) * 100));
                let hi = q.bind(Arg::Int(i64::from(*c) * 100 + 100));
                format!("(status_code >= {lo} AND status_code < {hi})")
            })
            .collect();
        q.push(&format!(" AND ({})", parts.join(" OR ")));
    }
    q.any_in_set("tags", &f.tags);
    q.any_in_set("guardrails", &f.guardrails);
    q.any_in_set("verdicts", &f.verdicts);
    q.any_in_set("categories", &f.categories);
    for (column, value) in [("flagged", f.flagged), ("stream", f.stream)] {
        if let Some(v) = value {
            let m = q.bind(Arg::Int(i64::from(v)));
            q.push(&format!(" AND {column} = {m}"));
        }
    }
    if let Some(v) = f.latency_min {
        let m = q.bind(Arg::Real(v));
        q.push(&format!(" AND latency_ms >= {m}"));
    }
    if let Some(v) = f.latency_max {
        let m = q.bind(Arg::Real(v));
        q.push(&format!(" AND latency_ms <= {m}"));
    }
    if f.cost_min_nusd.is_some() || f.cost_max_nusd.is_some() {
        q.push(" AND cost_nusd >= 0");
    }
    if let Some(v) = f.cost_min_nusd {
        let m = q.bind(Arg::Int(v));
        q.push(&format!(" AND cost_nusd >= {m}"));
    }
    if let Some(v) = f.cost_max_nusd {
        let m = q.bind(Arg::Int(v));
        q.push(&format!(" AND cost_nusd <= {m}"));
    }
    if let Some(text) = f.q.as_deref().filter(|t| !t.is_empty()) {
        let like = q.case_insensitive_like();
        let pattern = format!("%{}%", like_escape(text));
        let mut columns = vec![
            "id",
            "request_id",
            "end_user",
            "key_name",
            "model",
            "requested_model",
            "user_name",
        ];
        if f.q_content {
            columns.push("prompt");
        }
        let escape = q.escape_clause();
        let parts: Vec<String> = columns
            .iter()
            .map(|c| {
                let m = q.bind(Arg::Text(pattern.clone()));
                format!("{c} {like} {m}{escape}")
            })
            .collect();
        q.push(&format!(" AND ({})", parts.join(" OR ")));
    }
    if let Some(scopes) = &f.scopes {
        scope_clause(q, scopes, "organization", "team", "project");
    }
}

/// Restricts rows to `scopes` (none at all when the list is empty).
pub(crate) fn scope_clause(q: &mut Sql, scopes: &[ScopeRef], org: &str, team: &str, project: &str) {
    if scopes.is_empty() {
        q.push(" AND 1 = 0");
        return;
    }
    let parts: Vec<String> = scopes
        .iter()
        .map(|s| {
            let (column, name) = match s {
                ScopeRef::Organization(n) => (org, n),
                ScopeRef::Team(n) => (team, n),
                ScopeRef::Project(n) => (project, n),
            };
            let m = q.bind(Arg::Text(name.clone()));
            format!("{column} = {m}")
        })
        .collect();
    q.push(&format!(" AND ({})", parts.join(" OR ")));
}

/// The ordered list query of a filter: `SELECT id, started_us, record, content, <sort value>`.
pub(crate) fn list(pg: bool, f: &LogFilter, order: &LogOrder) -> Sql {
    let column = order.sort.column();
    let head = format!(
        "SELECT id, started_us, record, content, {column}, user_name, environment, route_node FROM request_logs"
    );
    list_with(Sql::new(pg, &head), f, order)
}

/// [`list`] on ClickHouse, reading the newest version of each record, with the sort value as both an
/// integer and a real.
pub(crate) fn list_clickhouse(f: &LogFilter, order: &LogOrder) -> Sql {
    let column = order.sort.column();
    let head = format!(
        "SELECT id, started_us, record, content, toInt64({column}) AS sort_int, toFloat64({column}) AS sort_real, \
         user_name, environment, route_node FROM request_logs FINAL"
    );
    list_with(Sql::in_dialect(Dialect::ClickHouse, &head), f, order)
}

fn list_with(mut q: Sql, f: &LogFilter, order: &LogOrder) -> Sql {
    let column = order.sort.column();
    filter(&mut q, f);
    let (cmp, dir) = if order.desc { ("<", "DESC") } else { (">", "ASC") };
    if let Some(after) = &order.after {
        if order.sort == SortKey::StartedAt {
            let s = q.bind(Arg::Int(after.started_us));
            let i = q.bind(Arg::Text(after.id.clone()));
            q.push(&format!(" AND (started_us, id) {cmp} ({s}, {i})"));
        } else {
            let v = q.bind(after.value.clone());
            let s = q.bind(Arg::Int(after.started_us));
            let i = q.bind(Arg::Text(after.id.clone()));
            q.push(&format!(" AND ({column}, started_us, id) {cmp} ({v}, {s}, {i})"));
        }
    }
    if order.sort == SortKey::StartedAt {
        q.push(&format!(" ORDER BY started_us {dir}, id {dir}"));
    } else {
        q.push(&format!(" ORDER BY {column} {dir}, started_us {dir}, id {dir}"));
    }
    let limit = q.bind(Arg::Int(i64::try_from(order.limit).unwrap_or(i64::MAX)));
    q.push(&format!(" LIMIT {limit}"));
    q
}

/// `SELECT COUNT(*)` over at most `cap + 1` matching rows.
pub(crate) fn count(pg: bool, f: &LogFilter, cap: u64) -> Sql {
    count_with(
        Sql::new(pg, "SELECT COUNT(*) FROM (SELECT 1 FROM request_logs"),
        f,
        cap,
    )
}

/// [`count`] on ClickHouse.
pub(crate) fn count_clickhouse(f: &LogFilter, cap: u64) -> Sql {
    let head = "SELECT toUInt64(COUNT(*)) FROM (SELECT 1 FROM request_logs FINAL";
    count_with(Sql::in_dialect(Dialect::ClickHouse, head), f, cap)
}

fn count_with(mut q: Sql, f: &LogFilter, cap: u64) -> Sql {
    filter(&mut q, f);
    let limit = q.bind(Arg::Int(i64::try_from(cap.saturating_add(1)).unwrap_or(i64::MAX)));
    q.push(&format!(" LIMIT {limit}) AS matching"));
    q
}

pub(crate) fn sort_value(sort: SortKey, int: Option<i64>, real: Option<f64>) -> Arg {
    if sort.real() {
        Arg::Real(real.unwrap_or(-1.0))
    } else {
        Arg::Int(int.unwrap_or(-1))
    }
}

pub(crate) fn sqlite_insert(c: &mut Connection, rows: &[LogRow]) -> Result<(), StoreError> {
    let tx = c.transaction()?;
    {
        let marks = vec!["?"; COLUMN_COUNT].join(", ");
        let mut stmt = tx.prepare(&format!(
            "INSERT OR IGNORE INTO request_logs ({COLUMNS}) VALUES ({marks})"
        ))?;
        for r in rows {
            let values: Vec<rusqlite::types::Value> = row_args(r)
                .into_iter()
                .map(|a| a.map_or(rusqlite::types::Value::Null, |a| a.sqlite()))
                .collect();
            stmt.execute(rusqlite::params_from_iter(values))?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn sqlite_list(
    c: &mut Connection,
    f: &LogFilter,
    order: &LogOrder,
) -> Result<Vec<ListedLog>, StoreError> {
    let q = list(false, f, order);
    let mut stmt = c.prepare(&q.text)?;
    let args: Vec<rusqlite::types::Value> = q.args.iter().map(Arg::sqlite).collect();
    let sort = order.sort;
    let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
        let value = if sort.real() {
            sort_value(sort, None, r.get::<_, Option<f64>>(4)?)
        } else {
            sort_value(sort, r.get::<_, Option<i64>>(4)?, None)
        };
        let id: String = r.get(0)?;
        let started_us: i64 = r.get(1)?;
        Ok(ListedLog {
            pos: SortPos {
                value,
                started_us,
                id: id.clone(),
            },
            log: StoredLog {
                id,
                started_us,
                record: r.get(2)?,
                content: r.get(3)?,
                user: r.get(5)?,
                environment: r.get(6)?,
                route_node: r.get(7)?,
            },
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub(crate) fn sqlite_count(c: &mut Connection, f: &LogFilter, cap: u64) -> Result<u64, StoreError> {
    let q = count(false, f, cap);
    let args: Vec<rusqlite::types::Value> = q.args.iter().map(Arg::sqlite).collect();
    let n: i64 = c.query_row(&q.text, rusqlite::params_from_iter(args), |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// The SQLite schema of `logs.db`.
pub(crate) const SQLITE_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS request_logs (
        id TEXT PRIMARY KEY,
        started_us INTEGER NOT NULL,
        node_id TEXT NOT NULL,
        request_id TEXT NOT NULL,
        organization TEXT, team TEXT, project TEXT, key_name TEXT, user_name TEXT,
        model TEXT, requested_model TEXT, provider TEXT, deployment TEXT, route TEXT, route_node TEXT,
        status_code INTEGER NOT NULL,
        code TEXT,
        response_path TEXT NOT NULL,
        operation TEXT NOT NULL,
        end_user TEXT, session_id TEXT, environment TEXT,
        tags TEXT NOT NULL, guardrails TEXT NOT NULL, verdicts TEXT NOT NULL, categories TEXT NOT NULL,
        flagged INTEGER NOT NULL, stream INTEGER NOT NULL,
        cache_status TEXT,
        latency_ms REAL NOT NULL, ttft_ms REAL NOT NULL, cost_nusd INTEGER NOT NULL,
        input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
        client_ip TEXT,
        record TEXT NOT NULL,
        content TEXT,
        prompt TEXT)",
    "CREATE INDEX IF NOT EXISTS request_logs_started ON request_logs (started_us, id)",
    "CREATE INDEX IF NOT EXISTS request_logs_key ON request_logs (key_name, started_us)",
    "CREATE INDEX IF NOT EXISTS request_logs_project ON request_logs (project, started_us)",
    "CREATE INDEX IF NOT EXISTS request_logs_team ON request_logs (team, started_us)",
    "CREATE INDEX IF NOT EXISTS request_logs_org ON request_logs (organization, started_us)",
    "CREATE INDEX IF NOT EXISTS request_logs_model ON request_logs (model, started_us)",
    "CREATE INDEX IF NOT EXISTS request_logs_user ON request_logs (user_name, started_us)",
    "CREATE INDEX IF NOT EXISTS request_logs_request ON request_logs (request_id)",
    "CREATE INDEX IF NOT EXISTS request_logs_end_user ON request_logs (end_user, started_us)",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_follow_the_backend() {
        let f = LogFilter {
            from_us: Some(1),
            status_classes: vec![5],
            tags: vec!["a%b".into()],
            q: Some("x".into()),
            scopes: Some(vec![ScopeRef::Team("t".into())]),
            ..LogFilter::default()
        };
        let order = LogOrder {
            sort: SortKey::CostUsd,
            desc: true,
            after: Some(SortPos {
                value: Arg::Int(5),
                started_us: 9,
                id: "i".into(),
            }),
            limit: 10,
        };
        let pg = list(true, &f, &order);
        assert!(pg.text.contains("$1") && !pg.text.contains('?'));
        assert!(pg.text.contains("ILIKE"));
        assert!(pg.text.contains("(cost_nusd, started_us, id) < ("));
        let lite = list(false, &f, &order);
        assert_eq!(lite.text.matches('?').count(), lite.args.len());
        assert!(lite.args.contains(&Arg::Text("%\u{1f}a\\%b\u{1f}%".into())));
        assert_eq!(row_args(&LogRow::default()).len(), COLUMN_COUNT);
        assert_eq!(COLUMNS.split(',').count(), COLUMN_COUNT);
    }
}

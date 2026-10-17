//! The request log on ClickHouse (`store.logs.backend: clickhouse`): the records the primary store
//! would keep, in one `ReplacingMergeTree` table ordered by start time, so a record changed after it
//! was written (an asynchronous guardrail's verdict) is a newer version of the same row and reads
//! see the newest one. Queries are the primary store's, in ClickHouse's dialect. The feedback on
//! records and the rollups stay in the primary store.

use std::time::Duration;

use clickhouse::sql::Identifier;
use clickhouse::{Client, Row};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use crate::StoreError;
use crate::logs::sql::{self, Arg, Sql};
use crate::logs::{ListedLog, LogColumn, LogFilter, LogOrder, LogRow, SortKey, SortPos, StoredLog};

/// How ClickHouse is reached and written.
#[derive(Clone)]
pub struct ClickHouseSettings {
    /// `http(s)://[user[:password]@]host:port`.
    pub url: antirion_core::Secret,
    pub database: String,
    pub batch_size: usize,
    pub flush_interval: Duration,
    pub insert_timeout: Duration,
}

impl std::fmt::Debug for ClickHouseSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClickHouseSettings")
            .field("database", &self.database)
            .field("batch_size", &self.batch_size)
            .field("flush_interval", &self.flush_interval)
            .finish_non_exhaustive()
    }
}

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS request_logs (
    id String, started_us Int64, node_id String, request_id String,
    organization Nullable(String), team Nullable(String), project Nullable(String),
    key_name Nullable(String), user_name Nullable(String),
    model Nullable(String), requested_model Nullable(String), provider Nullable(String),
    deployment Nullable(String), route Nullable(String), route_node Nullable(String),
    status_code Int64, code Nullable(String), response_path String, operation String,
    end_user Nullable(String), session_id Nullable(String), environment Nullable(String),
    tags String, guardrails String, verdicts String, categories String,
    flagged UInt8, stream UInt8, cache_status Nullable(String),
    latency_ms Float64, ttft_ms Float64, cost_nusd Int64, input_tokens Int64, output_tokens Int64,
    client_ip Nullable(String), record String, content Nullable(String), prompt Nullable(String),
    version UInt64)
ENGINE = ReplacingMergeTree(version)
PARTITION BY toYYYYMM(toDateTime(intDiv(started_us, 1000000)))
ORDER BY (started_us, id)";

/// One record as the table holds it.
#[derive(Clone, Debug, Default, PartialEq, Row, Serialize, Deserialize)]
struct Record {
    id: String,
    started_us: i64,
    node_id: String,
    request_id: String,
    organization: Option<String>,
    team: Option<String>,
    project: Option<String>,
    key_name: Option<String>,
    user_name: Option<String>,
    model: Option<String>,
    requested_model: Option<String>,
    provider: Option<String>,
    deployment: Option<String>,
    route: Option<String>,
    route_node: Option<String>,
    status_code: i64,
    code: Option<String>,
    response_path: String,
    operation: String,
    end_user: Option<String>,
    session_id: Option<String>,
    environment: Option<String>,
    tags: String,
    guardrails: String,
    verdicts: String,
    categories: String,
    flagged: u8,
    stream: u8,
    cache_status: Option<String>,
    latency_ms: f64,
    ttft_ms: f64,
    cost_nusd: i64,
    input_tokens: i64,
    output_tokens: i64,
    client_ip: Option<String>,
    record: String,
    content: Option<String>,
    prompt: Option<String>,
    version: u64,
}

impl From<LogRow> for Record {
    fn from(r: LogRow) -> Record {
        Record {
            id: r.id,
            started_us: r.started_us,
            node_id: r.node_id,
            request_id: r.request_id,
            organization: r.organization,
            team: r.team,
            project: r.project,
            key_name: r.key,
            user_name: r.user,
            model: r.model,
            requested_model: r.requested_model,
            provider: r.provider,
            deployment: r.deployment,
            route: r.route,
            route_node: r.route_node,
            status_code: r.status_code,
            code: r.code,
            response_path: r.response_path,
            operation: r.operation,
            end_user: r.end_user,
            session_id: r.session_id,
            environment: r.environment,
            tags: r.tags,
            guardrails: r.guardrails,
            verdicts: r.verdicts,
            categories: r.categories,
            flagged: u8::from(r.flagged),
            stream: u8::from(r.stream),
            cache_status: r.cache,
            latency_ms: r.latency_ms,
            ttft_ms: r.ttft_ms,
            cost_nusd: r.cost_nusd,
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            client_ip: r.client_ip,
            record: r.record,
            content: r.content,
            prompt: r.prompt,
            version: 0,
        }
    }
}

#[derive(Debug, Row, Deserialize)]
struct Listed {
    id: String,
    started_us: i64,
    record: String,
    content: Option<String>,
    sort_int: i64,
    sort_real: f64,
    user_name: Option<String>,
    environment: Option<String>,
    route_node: Option<String>,
}

fn err(e: &clickhouse::error::Error) -> StoreError {
    use clickhouse::error::Error as E;
    let outage = matches!(e, E::Network(_) | E::TimedOut);
    StoreError::LogStore {
        message: e.to_string(),
        outage,
    }
}

/// The parts of a URL the client takes apart: the base, the user and the password.
fn split_url(url: &str) -> Result<(String, Option<String>, Option<String>), StoreError> {
    let bad = || StoreError::Config("store.logs.clickhouse.url is not an http(s) URL".into());
    let (scheme, rest) = url.split_once("://").ok_or_else(bad)?;
    if !matches!(scheme, "http" | "https") {
        return Err(bad());
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let (user, password, host) = match authority.rsplit_once('@') {
        Some((info, host)) => match info.split_once(':') {
            Some((u, p)) => (Some(u.to_owned()), Some(p.to_owned()), host),
            None => (Some(info.to_owned()), None, host),
        },
        None => (None, None, authority),
    };
    if host.is_empty() {
        return Err(bad());
    }
    Ok((format!("{scheme}://{host}{path}"), user, password))
}

/// The request log in ClickHouse.
pub struct ClickHouseLogs {
    client: Client,
    /// The same server without a database, to create it.
    server: Client,
    database: String,
    settings: ClickHouseSettings,
    ready: OnceCell<()>,
}

impl std::fmt::Debug for ClickHouseLogs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClickHouseLogs")
            .field("database", &self.database)
            .finish_non_exhaustive()
    }
}

impl ClickHouseLogs {
    /// A store on the configured server; nothing is sent until the first call.
    pub fn new(settings: ClickHouseSettings) -> Result<ClickHouseLogs, StoreError> {
        let (base, user, password) = split_url(settings.url.expose())?;
        let mut server = Client::default().with_url(base);
        if let Some(u) = user {
            server = server.with_user(u);
        }
        if let Some(p) = password {
            server = server.with_password(p);
        }
        let client = server.clone().with_database(settings.database.clone());
        Ok(ClickHouseLogs {
            client,
            server,
            database: settings.database.clone(),
            settings,
            ready: OnceCell::new(),
        })
    }

    #[must_use]
    pub fn settings(&self) -> &ClickHouseSettings {
        &self.settings
    }

    /// Creates the database and the table on first use (again after a failure).
    async fn ready(&self) -> Result<(), StoreError> {
        self.ready
            .get_or_try_init(|| async {
                self.server
                    .query("CREATE DATABASE IF NOT EXISTS ?")
                    .bind(Identifier(&self.database))
                    .execute()
                    .await
                    .map_err(|e| err(&e))?;
                self.client.query(SCHEMA).execute().await.map_err(|e| err(&e))
            })
            .await
            .map(|_| ())
    }

    fn bound(&self, q: &Sql) -> clickhouse::query::Query {
        let mut query = self.client.query(&q.text);
        for a in &q.args {
            query = match a {
                Arg::Int(i) => query.bind(*i),
                Arg::Real(r) => query.bind(*r),
                Arg::Text(t) => query.bind(t.as_str()),
            };
        }
        query
    }

    /// Writes records in batches of `batch_size`; a record already stored is stored again, and
    /// reads keep one.
    pub async fn insert(&self, rows: Vec<LogRow>) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        self.ready().await?;
        let mut rows = rows.into_iter().map(Record::from).peekable();
        while rows.peek().is_some() {
            let mut insert = self
                .client
                .insert::<Record>("request_logs")
                .await
                .map_err(|e| err(&e))?
                .with_timeouts(
                    Some(self.settings.insert_timeout),
                    Some(self.settings.insert_timeout),
                );
            for r in rows.by_ref().take(self.settings.batch_size.max(1)) {
                insert.write(&r).await.map_err(|e| err(&e))?;
            }
            insert.end().await.map_err(|e| err(&e))?;
        }
        Ok(())
    }

    pub async fn list(&self, f: &LogFilter, order: &LogOrder) -> Result<Vec<ListedLog>, StoreError> {
        self.ready().await?;
        let q = sql::list_clickhouse(f, order);
        let rows: Vec<Listed> = self.bound(&q).fetch_all().await.map_err(|e| err(&e))?;
        let sort = order.sort;
        Ok(rows
            .into_iter()
            .map(|r| ListedLog {
                pos: SortPos {
                    value: sql::sort_value(sort, Some(r.sort_int), Some(r.sort_real)),
                    started_us: r.started_us,
                    id: r.id.clone(),
                },
                log: StoredLog {
                    id: r.id,
                    started_us: r.started_us,
                    record: r.record,
                    content: r.content,
                    user: r.user_name,
                    environment: r.environment,
                    route_node: r.route_node,
                },
            })
            .collect())
    }

    pub async fn count(&self, f: &LogFilter, cap: u64) -> Result<u64, StoreError> {
        self.ready().await?;
        self.bound(&sql::count_clickhouse(f, cap))
            .fetch_one::<u64>()
            .await
            .map_err(|e| err(&e))
    }

    pub async fn log(&self, id: &str) -> Result<Option<StoredLog>, StoreError> {
        let mut f = LogFilter::default();
        f.any_of.push((LogColumn::Id, vec![Arg::Text(id.to_owned())]));
        let order = LogOrder {
            sort: SortKey::StartedAt,
            desc: true,
            after: None,
            limit: 1,
        };
        Ok(self.list(&f, &order).await?.into_iter().next().map(|l| l.log))
    }

    /// Replaces a record's JSON with a newer version of its row; false when no record has that id.
    pub async fn update(&self, id: &str, record: String) -> Result<bool, StoreError> {
        self.ready().await?;
        let current: Option<Record> = self
            .client
            .query("SELECT ?fields FROM request_logs FINAL WHERE id = ? LIMIT 1")
            .bind(id)
            .fetch_optional()
            .await
            .map_err(|e| err(&e))?;
        let Some(mut row) = current else {
            return Ok(false);
        };
        row.record = record;
        row.version = row.version.saturating_add(1);
        let mut insert = self
            .client
            .insert::<Record>("request_logs")
            .await
            .map_err(|e| err(&e))?
            .with_timeouts(
                Some(self.settings.insert_timeout),
                Some(self.settings.insert_timeout),
            );
        insert.write(&row).await.map_err(|e| err(&e))?;
        insert.end().await.map_err(|e| err(&e))?;
        Ok(true)
    }

    /// Deletes the records that started before `before_us` and the content of those that started
    /// before `content_before_us`; returns how many records were deleted.
    pub async fn expire(&self, before_us: i64, content_before_us: i64) -> Result<u64, StoreError> {
        self.ready().await?;
        let f = LogFilter {
            to_us: Some(before_us),
            ..LogFilter::default()
        };
        let n = self.delete(&f).await?;
        self.client
            .query(
                "ALTER TABLE request_logs UPDATE content = NULL, prompt = NULL \
                 WHERE started_us < ? AND content IS NOT NULL",
            )
            .bind(content_before_us)
            .execute()
            .await
            .map_err(|e| err(&e))?;
        Ok(n)
    }

    /// Deletes every record `f` matches; returns how many.
    pub async fn delete(&self, f: &LogFilter) -> Result<u64, StoreError> {
        self.ready().await?;
        let n = self.count(f, u64::MAX - 1).await?;
        if n == 0 {
            return Ok(0);
        }
        let mut q = Sql::in_dialect(sql::Dialect::ClickHouse, "DELETE FROM request_logs");
        sql::filter(&mut q, f);
        self.bound(&q).execute().await.map_err(|e| err(&e))?;
        Ok(n)
    }

    /// The ids of the records `f` matches, for the state kept beside them elsewhere.
    pub async fn ids(&self, f: &LogFilter) -> Result<Vec<String>, StoreError> {
        self.ready().await?;
        let mut q = Sql::in_dialect(sql::Dialect::ClickHouse, "SELECT id FROM request_logs FINAL");
        sql::filter(&mut q, f);
        self.bound(&q).fetch_all::<String>().await.map_err(|e| err(&e))
    }

    /// Removes every record (test-mode reset only).
    pub async fn clear(&self) -> Result<(), StoreError> {
        self.ready().await?;
        self.client
            .query("TRUNCATE TABLE IF EXISTS request_logs")
            .execute()
            .await
            .map_err(|e| err(&e))
    }
}

impl crate::Store {
    /// Deletes the feedback of these records (their records live in ClickHouse).
    pub async fn delete_feedback_of(&self, ids: Vec<String>) -> Result<u64, StoreError> {
        let mut deleted = 0;
        for chunk in ids.chunks(500) {
            let mut q = Sql::new(
                matches!(self, crate::Store::Postgres(_)),
                "DELETE FROM log_feedback WHERE 1 = 1",
            );
            let values: Vec<Arg> = chunk.iter().cloned().map(Arg::Text).collect();
            q.any_of("log_id", &values);
            deleted += match self {
                crate::Store::Sqlite(s) => {
                    s.logs()
                        .call(move |c| {
                            let args: Vec<rusqlite::types::Value> = q.args.iter().map(Arg::sqlite).collect();
                            Ok(c.execute(&q.text, rusqlite::params_from_iter(args))? as u64)
                        })
                        .await?
                }
                crate::Store::Postgres(p) => {
                    let values: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> =
                        q.args.iter().map(Arg::pg).collect();
                    let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = values
                        .iter()
                        .map(|b| b.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
                        .collect();
                    p.execute(&q.text, &refs).await?
                }
            };
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests;

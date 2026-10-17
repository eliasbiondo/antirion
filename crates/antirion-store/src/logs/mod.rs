//! The request log and its rollups. A record is stored as indexed columns (what the list filters,
//! sorts and scopes on) beside its JSON, with its content in columns of their own so content can be
//! dropped earlier than the record. Rollups hold the request counts, usage, cost and latency sketches
//! per period (one minute, hour or day), per node and per combination of the dimensions analytics
//! group and filter on; each node writes only its own rows. In standalone mode both live in
//! `logs.db`, apart from the state database, so their volume never contends with it.

pub(crate) mod feedback;
pub(crate) mod rollup;
pub(crate) mod sql;

use crate::{Store, StoreError};

pub use feedback::FeedbackRow;
pub use rollup::{CACHE_PREFIX_FROM, MEASURES, ROLLUP_DIMS, RollupQuery, RollupRow, SKETCHES, SketchMerge};
pub use sql::Arg;

/// The statements that create `logs.db`.
pub(crate) fn sqlite_schema() -> Vec<String> {
    let mut out: Vec<String> = sql::SQLITE_SCHEMA.iter().map(|s| (*s).to_owned()).collect();
    out.push(rollup::sqlite_schema());
    out.extend(feedback::SQLITE_SCHEMA.iter().map(|s| (*s).to_owned()));
    out.extend(crate::safety::SQLITE_SCHEMA.iter().map(|s| (*s).to_owned()));
    out
}

/// Adds the rollup dimension columns a `logs.db` written by an older version lacks.
pub(crate) fn add_rollup_columns(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare("SELECT name FROM pragma_table_info('log_rollups')")?;
    let have: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    for d in ROLLUP_DIMS.iter().filter(|d| !have.iter().any(|h| h == *d)) {
        tx.execute_batch(&format!("ALTER TABLE log_rollups ADD COLUMN {d} TEXT"))?;
    }
    Ok(())
}

/// One request-log record as the store keeps it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogRow {
    pub id: String,
    pub started_us: i64,
    pub node_id: String,
    pub request_id: String,
    pub organization: Option<String>,
    pub team: Option<String>,
    pub project: Option<String>,
    pub key: Option<String>,
    /// The key's owner.
    pub user: Option<String>,
    pub model: Option<String>,
    pub requested_model: Option<String>,
    pub provider: Option<String>,
    pub deployment: Option<String>,
    pub route: Option<String>,
    /// The served leaf's path in the route tree.
    pub route_node: Option<String>,
    pub status_code: i64,
    pub code: Option<String>,
    pub response_path: String,
    pub operation: String,
    pub end_user: Option<String>,
    pub session_id: Option<String>,
    pub environment: Option<String>,
    /// Tags, guardrail names, verdicts and categories, each as a [`set`] value.
    pub tags: String,
    pub guardrails: String,
    pub verdicts: String,
    pub categories: String,
    pub flagged: bool,
    pub stream: bool,
    pub cache: Option<String>,
    pub latency_ms: f64,
    /// `-1` when the request had no first token.
    pub ttft_ms: f64,
    /// `-1` when the request was unpriced.
    pub cost_nusd: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub client_ip: Option<String>,
    /// The record's JSON without its content.
    pub record: String,
    /// The `content` member's JSON, when content was stored.
    pub content: Option<String>,
    /// The request side of the content, the text `q` searches for content readers.
    pub prompt: Option<String>,
}

/// A stored record: its JSON, its content and the facts kept beside the record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredLog {
    pub id: String,
    pub started_us: i64,
    pub record: String,
    pub content: Option<String>,
    pub user: Option<String>,
    pub environment: Option<String>,
    pub route_node: Option<String>,
}

/// Several values in one text column, matched one at a time with `LIKE`: each value is enclosed in
/// the unit separator, which never occurs in a name.
#[must_use]
pub fn set<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::from("\u{1f}");
    for v in values {
        out.extend(v.chars().filter(|c| *c != '\u{1f}'));
        out.push('\u{1f}');
    }
    out
}

/// The tenancy a principal may read records of: `None` is every record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeRef {
    Organization(String),
    Team(String),
    Project(String),
}

/// A column a log query filters with a list of accepted values.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LogColumn {
    Id,
    RequestId,
    Organization,
    Team,
    Project,
    Key,
    User,
    Model,
    RequestedModel,
    Provider,
    Deployment,
    Route,
    StatusCode,
    Code,
    ResponsePath,
    Operation,
    EndUser,
    SessionId,
    Environment,
    Cache,
}

impl LogColumn {
    fn sql(self) -> &'static str {
        match self {
            LogColumn::Id => "id",
            LogColumn::RequestId => "request_id",
            LogColumn::Organization => "organization",
            LogColumn::Team => "team",
            LogColumn::Project => "project",
            LogColumn::Key => "key_name",
            LogColumn::User => "user_name",
            LogColumn::Model => "model",
            LogColumn::RequestedModel => "requested_model",
            LogColumn::Provider => "provider",
            LogColumn::Deployment => "deployment",
            LogColumn::Route => "route",
            LogColumn::StatusCode => "status_code",
            LogColumn::Code => "code",
            LogColumn::ResponsePath => "response_path",
            LogColumn::Operation => "operation",
            LogColumn::EndUser => "end_user",
            LogColumn::SessionId => "session_id",
            LogColumn::Environment => "environment",
            LogColumn::Cache => "cache_status",
        }
    }
}

/// The filters of a log query; every member narrows it.
#[derive(Clone, Debug, Default)]
pub struct LogFilter {
    pub from_us: Option<i64>,
    pub to_us: Option<i64>,
    /// Each column matches one of its values; the columns are combined with AND.
    pub any_of: Vec<(LogColumn, Vec<Arg>)>,
    /// First digits of the status code (`5` for `5xx`).
    pub status_classes: Vec<u8>,
    pub tags: Vec<String>,
    pub guardrails: Vec<String>,
    pub verdicts: Vec<String>,
    pub categories: Vec<String>,
    pub flagged: Option<bool>,
    pub stream: Option<bool>,
    pub latency_min: Option<f64>,
    pub latency_max: Option<f64>,
    pub cost_min_nusd: Option<i64>,
    pub cost_max_nusd: Option<i64>,
    /// A case-insensitive substring of the id, request id, end user, key, Models and key owner.
    pub q: Option<String>,
    /// `q` also searches stored prompts.
    pub q_content: bool,
    /// Records of these scopes only; `None` for every record.
    pub scopes: Option<Vec<ScopeRef>>,
}

/// A sortable column.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SortKey {
    StartedAt,
    LatencyMs,
    TtftMs,
    CostUsd,
    InputTokens,
    OutputTokens,
    StatusCode,
}

impl SortKey {
    #[must_use]
    pub fn parse(s: &str) -> Option<SortKey> {
        Some(match s {
            "started_at" => SortKey::StartedAt,
            "latency_ms" => SortKey::LatencyMs,
            "ttft_ms" => SortKey::TtftMs,
            "cost_usd" => SortKey::CostUsd,
            "input_tokens" => SortKey::InputTokens,
            "output_tokens" => SortKey::OutputTokens,
            "status_code" => SortKey::StatusCode,
            _ => return None,
        })
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            SortKey::StartedAt => "started_at",
            SortKey::LatencyMs => "latency_ms",
            SortKey::TtftMs => "ttft_ms",
            SortKey::CostUsd => "cost_usd",
            SortKey::InputTokens => "input_tokens",
            SortKey::OutputTokens => "output_tokens",
            SortKey::StatusCode => "status_code",
        }
    }

    fn column(self) -> &'static str {
        match self {
            SortKey::StartedAt => "started_us",
            SortKey::LatencyMs => "latency_ms",
            SortKey::TtftMs => "ttft_ms",
            SortKey::CostUsd => "cost_nusd",
            SortKey::InputTokens => "input_tokens",
            SortKey::OutputTokens => "output_tokens",
            SortKey::StatusCode => "status_code",
        }
    }

    /// Whether the column holds real numbers rather than integers.
    #[must_use]
    pub fn real(self) -> bool {
        matches!(self, SortKey::LatencyMs | SortKey::TtftMs)
    }
}

/// Where a page starts: the sort value, `started_us` and id of the last record of the page before.
#[derive(Clone, Debug, PartialEq)]
pub struct SortPos {
    pub value: Arg,
    pub started_us: i64,
    pub id: String,
}

/// Order and position of a listing.
#[derive(Clone, Debug)]
pub struct LogOrder {
    pub sort: SortKey,
    pub desc: bool,
    pub after: Option<SortPos>,
    pub limit: usize,
}

/// A listed record with its sort position.
#[derive(Clone, Debug, PartialEq)]
pub struct ListedLog {
    pub log: StoredLog,
    pub pos: SortPos,
}

impl Store {
    /// Stores records; a record already stored is kept.
    pub async fn insert_logs(&self, rows: Vec<LogRow>) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        match self {
            Store::Sqlite(s) => s.logs().call(move |c| sql::sqlite_insert(c, &rows)).await,
            Store::Postgres(p) => p.insert_log_rows(rows).await,
        }
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
        Ok(self.list_logs(f, order).await?.into_iter().next().map(|l| l.log))
    }

    /// At most `order.limit` records matching `f`, in order.
    pub async fn list_logs(&self, f: LogFilter, order: LogOrder) -> Result<Vec<ListedLog>, StoreError> {
        match self {
            Store::Sqlite(s) => s.logs().call(move |c| sql::sqlite_list(c, &f, &order)).await,
            Store::Postgres(p) => p.list_log_rows(&f, &order).await,
        }
    }

    /// How many records match, counting at most `cap + 1`.
    pub async fn count_logs(&self, f: LogFilter, cap: u64) -> Result<u64, StoreError> {
        match self {
            Store::Sqlite(s) => s.logs().call(move |c| sql::sqlite_count(c, &f, cap)).await,
            Store::Postgres(p) => p.count_log_rows(&f, cap).await,
        }
    }

    /// Replaces a stored record's JSON; false when no record has that id.
    pub async fn update_log(&self, id: &str, record: String) -> Result<bool, StoreError> {
        let id = id.to_owned();
        match self {
            Store::Sqlite(s) => {
                s.logs()
                    .call(move |c| {
                        Ok(c.execute(
                            "UPDATE request_logs SET record = ?2 WHERE id = ?1",
                            rusqlite::params![id, record],
                        )? > 0)
                    })
                    .await
            }
            Store::Postgres(p) => p
                .execute(
                    "UPDATE request_logs SET record = $2 WHERE id = $1",
                    &[&id, &record],
                )
                .await
                .map(|n| n > 0),
        }
    }

    /// Deletes the records and rollups that started before `before_us`, and the content of records
    /// that started before `content_before_us`; returns how many records were deleted.
    pub async fn expire_logs(&self, before_us: i64, content_before_us: i64) -> Result<u64, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.logs()
                    .call(move |c| {
                        let n = c.execute("DELETE FROM request_logs WHERE started_us < ?1", [before_us])?;
                        c.execute(
                            "UPDATE request_logs SET content = NULL, prompt = NULL
                             WHERE started_us < ?1 AND content IS NOT NULL",
                            [content_before_us],
                        )?;
                        c.execute("DELETE FROM log_rollups WHERE start_us < ?1", [before_us])?;
                        c.execute("DELETE FROM log_feedback WHERE created_us < ?1", [before_us])?;
                        Ok(n as u64)
                    })
                    .await
            }
            Store::Postgres(p) => p.expire_log_rows(before_us, content_before_us).await,
        }
    }

    /// Removes every record and rollup (test-mode reset only).
    pub async fn clear_logs(&self) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.logs()
                    .call(|c| {
                        c.execute_batch(
                            "DELETE FROM request_logs; DELETE FROM log_rollups; DELETE FROM log_feedback;",
                        )?;
                        Ok(())
                    })
                    .await
            }
            Store::Postgres(p) => {
                p.execute("DELETE FROM request_logs", &[]).await?;
                p.execute("DELETE FROM log_feedback", &[]).await?;
                p.execute("DELETE FROM log_rollups", &[]).await.map(|_| ())
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;

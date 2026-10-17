//! Feedback attached to request-log records, and the erasure of an end user's records (with their
//! feedback). Feedback lives beside the records, in `logs.db` or the cluster's `log_feedback` table.

use crate::logs::LogFilter;
use crate::logs::sql::{self, Arg, Sql};
use crate::{Store, StoreError};

/// The SQLite table of feedback.
pub(crate) const SQLITE_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS log_feedback (
        id TEXT PRIMARY KEY, log_id TEXT NOT NULL, metric TEXT NOT NULL, value TEXT NOT NULL,
        created_us INTEGER NOT NULL)",
    "CREATE INDEX IF NOT EXISTS log_feedback_log ON log_feedback (log_id, created_us)",
];

/// The PostgreSQL migration of feedback.
pub(crate) const PG_MIGRATION: &str = "CREATE TABLE IF NOT EXISTS log_feedback (
        id text PRIMARY KEY, log_id text NOT NULL, metric text NOT NULL, value text NOT NULL,
        created_us bigint NOT NULL);
     CREATE INDEX IF NOT EXISTS log_feedback_log ON log_feedback (log_id, created_us);";

/// One piece of feedback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackRow {
    pub id: String,
    pub log_id: String,
    pub metric: String,
    /// The value as JSON.
    pub value: String,
    pub created_us: i64,
}

/// The statements that erase what `f` matches: feedback first, then the records.
fn erase(pg: bool, f: &LogFilter) -> [Sql; 2] {
    let mut feedback = Sql::new(
        pg,
        "DELETE FROM log_feedback WHERE log_id IN (SELECT id FROM request_logs",
    );
    sql::filter(&mut feedback, f);
    feedback.push(")");
    let mut records = Sql::new(pg, "DELETE FROM request_logs");
    sql::filter(&mut records, f);
    [feedback, records]
}

impl Store {
    /// Stores one piece of feedback.
    pub async fn insert_feedback(&self, row: FeedbackRow) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.logs()
                    .call(move |c| {
                        c.execute(
                            "INSERT INTO log_feedback (id, log_id, metric, value, created_us)
                             VALUES (?1, ?2, ?3, ?4, ?5)",
                            rusqlite::params![row.id, row.log_id, row.metric, row.value, row.created_us],
                        )?;
                        Ok(())
                    })
                    .await
            }
            Store::Postgres(p) => p
                .execute(
                    "INSERT INTO log_feedback (id, log_id, metric, value, created_us)
                     VALUES ($1, $2, $3, $4, $5)",
                    &[&row.id, &row.log_id, &row.metric, &row.value, &row.created_us],
                )
                .await
                .map(|_| ()),
        }
    }

    /// The feedback of one record, oldest first.
    pub async fn feedback_of(&self, log_id: &str) -> Result<Vec<FeedbackRow>, StoreError> {
        const SELECT: &str = "SELECT id, log_id, metric, value, created_us FROM log_feedback";
        let id = log_id.to_owned();
        match self {
            Store::Sqlite(s) => {
                s.logs()
                    .call(move |c| {
                        let mut stmt =
                            c.prepare(&format!("{SELECT} WHERE log_id = ?1 ORDER BY created_us, id"))?;
                        let rows = stmt.query_map([id], |r| {
                            Ok(FeedbackRow {
                                id: r.get(0)?,
                                log_id: r.get(1)?,
                                metric: r.get(2)?,
                                value: r.get(3)?,
                                created_us: r.get(4)?,
                            })
                        })?;
                        Ok(rows.collect::<Result<Vec<_>, _>>()?)
                    })
                    .await
            }
            Store::Postgres(p) => {
                let rows = p
                    .rows(
                        &format!("{SELECT} WHERE log_id = $1 ORDER BY created_us, id"),
                        &[&id],
                    )
                    .await?;
                rows.iter()
                    .map(|r| {
                        Ok(FeedbackRow {
                            id: r.try_get(0)?,
                            log_id: r.try_get(1)?,
                            metric: r.try_get(2)?,
                            value: r.try_get(3)?,
                            created_us: r.try_get(4)?,
                        })
                    })
                    .collect()
            }
        }
    }

    /// Deletes every record `f` matches, with its feedback; returns how many records were deleted.
    pub async fn delete_logs(&self, f: LogFilter) -> Result<u64, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.logs()
                    .call(move |c| {
                        let tx = c.transaction()?;
                        let mut n = 0;
                        for q in erase(false, &f) {
                            let args: Vec<rusqlite::types::Value> = q.args.iter().map(Arg::sqlite).collect();
                            n = tx.execute(&q.text, rusqlite::params_from_iter(args))?;
                        }
                        tx.commit()?;
                        Ok(u64::try_from(n).unwrap_or(0))
                    })
                    .await
            }
            Store::Postgres(p) => {
                let mut n = 0;
                for q in erase(true, &f) {
                    let values: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> =
                        q.args.iter().map(Arg::pg).collect();
                    let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = values
                        .iter()
                        .map(|b| b.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
                        .collect();
                    n = p.execute(&q.text, &refs).await?;
                }
                Ok(n)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::LogColumn;

    #[test]
    fn erasure_deletes_feedback_of_the_same_selection_first() {
        let mut f = LogFilter::default();
        f.any_of.push((LogColumn::EndUser, vec![Arg::Text("u-1".into())]));
        let [feedback, records] = erase(true, &f);
        assert!(
            feedback
                .text
                .starts_with("DELETE FROM log_feedback WHERE log_id IN (SELECT id FROM request_logs WHERE")
        );
        assert!(feedback.text.contains("end_user IN ($1)") && feedback.text.ends_with(')'));
        assert!(records.text.starts_with("DELETE FROM request_logs WHERE"));
        assert_eq!(records.args.len(), 1);
    }
}

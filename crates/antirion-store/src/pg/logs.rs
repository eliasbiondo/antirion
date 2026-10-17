//! The request log and its rollups on PostgreSQL.

use tokio_postgres::types::ToSql;

use super::PgStore;
use crate::StoreError;
use crate::logs::rollup::{self, SketchMerge};
use crate::logs::sql::{self, Arg, COLUMN_COUNT, COLUMNS};
use crate::logs::{
    ListedLog, LogFilter, LogOrder, LogRow, RollupQuery, RollupRow, SKETCHES, SortPos, StoredLog,
};

type Param = Box<dyn ToSql + Sync + Send>;

fn params(args: &[Arg]) -> Vec<Param> {
    args.iter().map(Arg::pg).collect()
}

fn refs(p: &[Param]) -> Vec<&(dyn ToSql + Sync)> {
    p.iter().map(|b| b.as_ref() as &(dyn ToSql + Sync)).collect()
}

fn nullable(a: Option<Arg>) -> Param {
    match a {
        Some(a) => a.pg(),
        None => Box::new(Option::<String>::None),
    }
}

impl PgStore {
    pub(crate) async fn insert_log_rows(&self, rows: Vec<LogRow>) -> Result<(), StoreError> {
        let marks: Vec<String> = (1..=COLUMN_COUNT).map(|i| format!("${i}")).collect();
        let statement = format!(
            "INSERT INTO request_logs ({COLUMNS}) VALUES ({}) ON CONFLICT (id) DO NOTHING",
            marks.join(", ")
        );
        self.run(async {
            let mut lease = self.pool.get().await?;
            let client = lease.client()?;
            let tx = client.transaction().await?;
            let prepared = tx.prepare(&statement).await?;
            for r in &rows {
                let values: Vec<Param> = sql::row_args(r).into_iter().map(nullable).collect();
                tx.execute(&prepared, &refs(&values)).await?;
            }
            tx.commit().await?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn list_log_rows(
        &self,
        f: &LogFilter,
        order: &LogOrder,
    ) -> Result<Vec<ListedLog>, StoreError> {
        let q = sql::list(true, f, order);
        let values = params(&q.args);
        let rows = self.rows(&q.text, &refs(&values)).await?;
        let sort = order.sort;
        rows.iter()
            .map(|r| {
                let value = if sort.real() {
                    sql::sort_value(sort, None, r.try_get::<_, Option<f64>>(4)?)
                } else {
                    sql::sort_value(sort, r.try_get::<_, Option<i64>>(4)?, None)
                };
                let id: String = r.try_get(0)?;
                let started_us: i64 = r.try_get(1)?;
                Ok(ListedLog {
                    pos: SortPos {
                        value,
                        started_us,
                        id: id.clone(),
                    },
                    log: StoredLog {
                        id,
                        started_us,
                        record: r.try_get(2)?,
                        content: r.try_get(3)?,
                        user: r.try_get(5)?,
                        environment: r.try_get(6)?,
                        route_node: r.try_get(7)?,
                    },
                })
            })
            .collect()
    }

    pub(crate) async fn count_log_rows(&self, f: &LogFilter, cap: u64) -> Result<u64, StoreError> {
        let q = sql::count(true, f, cap);
        let values = params(&q.args);
        let rows = self.rows(&q.text, &refs(&values)).await?;
        let n: i64 = rows.first().map(|r| r.try_get(0)).transpose()?.unwrap_or(0);
        Ok(u64::try_from(n).unwrap_or(0))
    }

    pub(crate) async fn expire_log_rows(
        &self,
        before_us: i64,
        content_before_us: i64,
    ) -> Result<u64, StoreError> {
        let n = self
            .execute("DELETE FROM request_logs WHERE started_us < $1", &[&before_us])
            .await?;
        self.execute(
            "UPDATE request_logs SET content = NULL, prompt = NULL
             WHERE started_us < $1 AND content IS NOT NULL",
            &[&content_before_us],
        )
        .await?;
        self.execute("DELETE FROM log_rollups WHERE start_us < $1", &[&before_us])
            .await?;
        self.execute("DELETE FROM log_feedback WHERE created_us < $1", &[&before_us])
            .await?;
        Ok(n)
    }

    pub(crate) async fn merge_rollup_rows(
        &self,
        rows: Vec<RollupRow>,
        merge: SketchMerge,
    ) -> Result<(), StoreError> {
        let find = format!(
            "SELECT {} FROM log_rollups WHERE grain_s = $1 AND start_us = $2 AND node_id = $3 AND dims_key = $4",
            rollup::columns()
        );
        let write = rollup::upsert(true);
        self.run(async {
            let mut lease = self.pool.get().await?;
            let client = lease.client()?;
            let tx = client.transaction().await?;
            let find = tx.prepare(&find).await?;
            let write = tx.prepare(&write).await?;
            for new in &rows {
                let key = new.dims_key();
                let stored = tx
                    .query_opt(&find, &[&new.grain_s, &new.start_us, &new.node_id, &key])
                    .await?
                    .map(|r| rollup::pg_row(&r))
                    .transpose()?;
                let row = match stored {
                    Some(mut s) => {
                        rollup::add_into(&mut s, new, merge);
                        s
                    }
                    None => new.clone(),
                };
                let mut values: Vec<Param> = rollup::args(&row).into_iter().map(nullable).collect();
                values.extend(
                    (0..SKETCHES.len())
                        .map(|i| Box::new(row.sketches.get(i).cloned().unwrap_or_default()) as Param),
                );
                tx.execute(&write, &refs(&values)).await?;
            }
            tx.commit().await?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn rollup_rows(&self, q: &RollupQuery) -> Result<Vec<RollupRow>, StoreError> {
        let s = rollup::query(true, q);
        let values = params(&s.args);
        let rows = self.rows(&s.text, &refs(&values)).await?;
        rows.iter().map(rollup::pg_row).collect()
    }
}

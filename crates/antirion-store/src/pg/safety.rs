//! Safety events and guardrail hours on PostgreSQL.

use tokio_postgres::types::ToSql;

use super::PgStore;
use crate::StoreError;
use crate::logs::SketchMerge;
use crate::logs::sql::Arg;
use crate::safety::{self, GuardrailHour, SafetyEventRow};

type Param = Box<dyn ToSql + Sync + Send>;

fn params(args: &[Arg]) -> Vec<Param> {
    args.iter().map(Arg::pg).collect()
}

fn refs(p: &[Param]) -> Vec<&(dyn ToSql + Sync)> {
    p.iter().map(|b| b.as_ref() as &(dyn ToSql + Sync)).collect()
}

fn nullable(a: Option<Arg>, real: bool) -> Param {
    match (a, real) {
        (Some(a), _) => a.pg(),
        (None, true) => Box::new(Option::<f64>::None),
        (None, false) => Box::new(Option::<String>::None),
    }
}

/// The event columns that hold numbers, by position, so a NULL is bound with the column's type.
const REAL_COLUMNS: [usize; 2] = [12, 23];

impl PgStore {
    pub(crate) async fn insert_safety_rows(&self, rows: Vec<SafetyEventRow>) -> Result<(), StoreError> {
        let marks: Vec<String> = (1..=28).map(|i| format!("${i}")).collect();
        let statement = format!(
            "INSERT INTO safety_events ({}) VALUES ({}) ON CONFLICT (id) DO NOTHING",
            safety::COLUMNS,
            marks.join(", ")
        );
        self.run(async {
            let mut lease = self.pool.get().await?;
            let client = lease.client()?;
            let tx = client.transaction().await?;
            let prepared = tx.prepare(&statement).await?;
            for r in &rows {
                let values: Vec<Param> = safety::args(r)
                    .into_iter()
                    .enumerate()
                    .map(|(i, a)| match (i, a) {
                        (26, None) => Box::new(Option::<i64>::None) as Param,
                        (i, a) => nullable(a, REAL_COLUMNS.contains(&i)),
                    })
                    .collect();
                tx.execute(&prepared, &refs(&values)).await?;
            }
            tx.commit().await?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn safety_rows(
        &self,
        sql: &str,
        args: &[Arg],
    ) -> Result<Vec<SafetyEventRow>, StoreError> {
        let values = params(args);
        self.rows(sql, &refs(&values))
            .await?
            .iter()
            .map(safety::pg_row)
            .collect()
    }

    pub(crate) async fn safety_count(&self, sql: &str, args: &[Arg]) -> Result<i64, StoreError> {
        let values = params(args);
        let rows = self.rows(sql, &refs(&values)).await?;
        Ok(rows.first().map(|r| r.try_get(0)).transpose()?.unwrap_or(0))
    }

    pub(crate) async fn safety_execute(&self, sql: &str, args: &[Arg]) -> Result<u64, StoreError> {
        let values = params(args);
        self.execute(sql, &refs(&values)).await
    }

    pub(crate) async fn merge_hour_rows(
        &self,
        rows: Vec<GuardrailHour>,
        merge: SketchMerge,
    ) -> Result<(), StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let client = lease.client()?;
            let tx = client.transaction().await?;
            let find = tx.prepare(safety::HOUR_FIND).await?;
            let write = tx.prepare(safety::HOUR_UPSERT).await?;
            for new in &rows {
                let stored = tx
                    .query_opt(&find, &[&new.guardrail, &new.hour_us, &new.node_id])
                    .await?
                    .map(|r| safety::hour_pg(&r))
                    .transpose()?;
                let row = match stored {
                    Some(mut s) => {
                        s.add(new, merge);
                        s
                    }
                    None => new.clone(),
                };
                tx.execute(
                    &write,
                    &[
                        &row.guardrail,
                        &row.hour_us,
                        &row.node_id,
                        &row.evaluations,
                        &row.hits,
                        &row.blocks,
                        &row.redactions,
                        &row.flags,
                        &row.errors,
                        &row.latency,
                    ],
                )
                .await?;
            }
            tx.commit().await?;
            Ok(())
        })
        .await
    }
}

//! The spend ledger: one row per line, idempotent by `(request_id, line)`. Lines are stored as the
//! JSON the Admin API shows; the store reads only each line's budget charges out of it, into
//! `ledger_charges`, indexed by counter and admission time for budget rebuilds.

use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreError};

pub(crate) const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS ledger_lines (
        request_id TEXT NOT NULL,
        line INTEGER NOT NULL,
        admitted_us INTEGER NOT NULL,
        body TEXT NOT NULL,
        PRIMARY KEY (request_id, line))",
    "CREATE INDEX IF NOT EXISTS ledger_lines_admitted ON ledger_lines (admitted_us)",
];

/// The charges table and the charges of the lines stored before it existed.
pub(crate) const CHARGES: &str = "CREATE TABLE ledger_charges (
        counter TEXT NOT NULL,
        admitted_us INTEGER NOT NULL,
        request_id TEXT NOT NULL,
        line INTEGER NOT NULL,
        amount INTEGER NOT NULL,
        PRIMARY KEY (counter, admitted_us, request_id, line)) WITHOUT ROWID;
    INSERT OR IGNORE INTO ledger_charges (counter, admitted_us, request_id, line, amount)
        SELECT json_extract(b.value, '$.counter'), l.admitted_us, l.request_id, l.line,
               CAST(json_extract(b.value, '$.amount') AS INTEGER)
        FROM ledger_lines l,
             json_each(CASE WHEN json_valid(l.body) THEN l.body ELSE '{}' END, '$.budgets') b
        WHERE json_extract(b.value, '$.counter') IS NOT NULL;";

const INSERT_CHARGES: &str =
    "INSERT OR IGNORE INTO ledger_charges (counter, admitted_us, request_id, line, amount)
    SELECT json_extract(b.value, '$.counter'), ?1, ?2, ?3, CAST(json_extract(b.value, '$.amount') AS INTEGER)
    FROM json_each(CASE WHEN json_valid(?4) THEN ?4 ELSE '{}' END, '$.budgets') b
    WHERE json_extract(b.value, '$.counter') IS NOT NULL";

/// One ledger line as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerRow {
    pub request_id: String,
    pub line: u32,
    pub admitted_us: i64,
    pub body: String,
    /// Whether the line charges a budget, when its writer knows; otherwise the store reads it from
    /// `body`.
    pub charges: Option<bool>,
}

/// Whether `body` can hold budget charges: anything but a valid document whose `budgets` member is
/// absent or `[]`, which the charges statement would find nothing in. Most lines charge no budget,
/// and SQLite would parse each of them whole to learn it.
fn may_charge(body: &str) -> bool {
    match antirion_json::skim(body.as_bytes(), &antirion_json::JsonLimits::default()) {
        Ok(doc) => doc
            .member("budgets")
            .is_some_and(|m| body.get(m.value.clone()) != Some("[]")),
        Err(_) => true,
    }
}

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<LedgerRow> {
    Ok(LedgerRow {
        request_id: r.get(0)?,
        line: r.get(1)?,
        admitted_us: r.get(2)?,
        body: r.get(3)?,
        charges: None,
    })
}

impl Store {
    /// Appends ledger lines in one transaction; lines already stored are kept as they are.
    pub async fn append_ledger(&self, rows: Vec<LedgerRow>) -> Result<(), StoreError> {
        let s = match self {
            Store::Sqlite(s) => s,
            Store::Postgres(p) => return p.append_ledger(rows).await,
        };
        s.call(move |c| {
            let tx = c.transaction()?;
            {
                let mut line = tx.prepare(
                    "INSERT OR IGNORE INTO ledger_lines (request_id, line, admitted_us, body) VALUES (?1, ?2, ?3, ?4)",
                )?;
                let mut charges = tx.prepare(INSERT_CHARGES)?;
                for r in &rows {
                    if line.execute(params![r.request_id, r.line, r.admitted_us, r.body])? > 0
                        && r.charges.unwrap_or_else(|| may_charge(&r.body))
                    {
                        charges.execute(params![r.admitted_us, r.request_id, r.line, r.body])?;
                    }
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Every stored line, oldest admission first.
    pub async fn ledger(&self) -> Result<Vec<LedgerRow>, StoreError> {
        self.ledger_between(None, None).await
    }

    /// The stored lines admitted at or after `from` and before `to`, oldest admission first.
    pub async fn ledger_between(
        &self,
        from: Option<i64>,
        to: Option<i64>,
    ) -> Result<Vec<LedgerRow>, StoreError> {
        let (from, to) = (from.unwrap_or(i64::MIN), to.unwrap_or(i64::MAX));
        let s = match self {
            Store::Sqlite(s) => s,
            Store::Postgres(p) => return p.ledger_between(from, to).await,
        };
        s.call(move |c| {
            let mut stmt = c.prepare(
                "SELECT request_id, line, admitted_us, body FROM ledger_lines
                 WHERE admitted_us >= ?1 AND admitted_us < ?2
                 ORDER BY admitted_us, request_id, line",
            )?;
            let rows = stmt.query_map(params![from, to], row_of)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    /// The stored lines of one request, by line number.
    pub async fn ledger_request(&self, request_id: &str) -> Result<Vec<LedgerRow>, StoreError> {
        let s = match self {
            Store::Sqlite(s) => s,
            Store::Postgres(p) => return p.ledger_request(request_id).await,
        };
        let id = request_id.to_owned();
        s.call(move |c| {
            let mut stmt = c.prepare(
                "SELECT request_id, line, admitted_us, body FROM ledger_lines WHERE request_id = ?1 ORDER BY line",
            )?;
            let rows = stmt.query_map(params![id], row_of)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    /// The exact sum of the charges to budget counter `counter` (its hex identity) by the stored
    /// lines admitted at or after `from` and before `to`.
    pub async fn ledger_charged(
        &self,
        counter: &str,
        from: i64,
        to: Option<i64>,
    ) -> Result<i128, StoreError> {
        let to = to.unwrap_or(i64::MAX);
        let s = match self {
            Store::Sqlite(s) => s,
            Store::Postgres(p) => return p.ledger_charged(counter, from, to).await,
        };
        let counter = counter.to_owned();
        s.call(move |c| {
            let range = "FROM ledger_charges WHERE counter = ?1 AND admitted_us >= ?2 AND admitted_us < ?3";
            let summed: Result<Option<i64>, _> = c
                .query_row(
                    &format!("SELECT SUM(amount) {range}"),
                    params![counter, from, to],
                    |r| r.get(0),
                )
                .optional()
                .map(Option::flatten);
            if let Ok(total) = summed {
                return Ok(i128::from(total.unwrap_or(0)));
            }
            let mut stmt = c.prepare(&format!("SELECT amount {range}"))?;
            let amounts = stmt.query_map(params![counter, from, to], |r| r.get::<_, i64>(0))?;
            let mut total = 0i128;
            for a in amounts {
                total = total.saturating_add(i128::from(a?));
            }
            Ok(total)
        })
        .await
    }

    /// The charges to budget counter `counter` by the stored lines admitted at or after `from`,
    /// summed by sub-window of `sub_us` microseconds (`admitted_us / sub_us`).
    pub async fn ledger_charged_slots(
        &self,
        counter: &str,
        from: i64,
        sub_us: i64,
    ) -> Result<Vec<(i64, i128)>, StoreError> {
        let sub_us = sub_us.max(1);
        let s = match self {
            Store::Sqlite(s) => s,
            Store::Postgres(p) => return p.ledger_charged_slots(counter, from, sub_us).await,
        };
        let counter = counter.to_owned();
        s.call(move |c| {
            let mut stmt = c.prepare(
                "SELECT admitted_us / ?3, amount FROM ledger_charges WHERE counter = ?1 AND admitted_us >= ?2",
            )?;
            let rows = stmt.query_map(params![counter, from, sub_us], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })?;
            let mut slots = std::collections::BTreeMap::<i64, i128>::new();
            for row in rows {
                let (k, amount) = row?;
                let slot = slots.entry(k).or_insert(0);
                *slot = slot.saturating_add(i128::from(amount));
            }
            Ok(slots.into_iter().collect())
        })
        .await
    }

    /// Removes every ledger line (test-mode reset only).
    pub async fn clear_ledger(&self) -> Result<(), StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(|c| Ok(c.execute_batch("DELETE FROM ledger_lines; DELETE FROM ledger_charges;")?))
                    .await
            }
            Store::Postgres(p) => p.clear_ledger().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(request_id: &str, line: u32, admitted_us: i64, body: &str) -> LedgerRow {
        LedgerRow {
            request_id: request_id.into(),
            line,
            admitted_us,
            body: body.into(),
            charges: None,
        }
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "antirion-store-{tag}-{:x}",
            antirion_core::ResourceUid::random().0
        ))
    }

    #[tokio::test]
    async fn lines_are_kept_once_and_in_admission_order() {
        let dir = scratch("ledger");
        let store = Store::open_embedded(&dir).unwrap();
        store
            .append_ledger(vec![row("b", 0, 20, "{\"b\":0}"), row("a", 0, 10, "{\"a\":0}")])
            .await
            .unwrap();
        store
            .append_ledger(vec![row("a", 0, 10, "{\"changed\":true}"), row("a", 1, 10, "{}")])
            .await
            .unwrap();
        let rows = store.ledger().await.unwrap();
        let ids: Vec<(&str, u32)> = rows.iter().map(|r| (r.request_id.as_str(), r.line)).collect();
        assert_eq!(ids, [("a", 0), ("a", 1), ("b", 0)]);
        assert_eq!(rows[0].body, "{\"a\":0}");
        assert_eq!(store.ledger_between(Some(11), None).await.unwrap().len(), 1);
        assert_eq!(store.ledger_between(None, Some(11)).await.unwrap().len(), 2);
        assert_eq!(store.ledger_request("a").await.unwrap().len(), 2);
        store.clear_ledger().await.unwrap();
        assert!(store.ledger().await.unwrap().is_empty());
        store.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn charged(counter: &str, amount: &str) -> String {
        format!(
            r#"{{"budgets":[{{"counter":"{counter}","amount":"{amount}"}},{{"counter":"other","amount":"5"}}]}}"#
        )
    }

    #[tokio::test]
    async fn a_writer_that_knows_the_charges_spares_reading_the_body() {
        let dir = scratch("known");
        let store = Store::open_embedded(&dir).unwrap();
        let known = |id: &str, charges: bool| LedgerRow {
            charges: Some(charges),
            ..row(id, 0, 10, &charged("c", "4"))
        };
        store
            .append_ledger(vec![
                known("r1", true),
                known("r2", false),
                row("r3", 0, 10, &charged("c", "1")),
            ])
            .await
            .unwrap();
        assert_eq!(store.ledger_charged("c", 0, None).await.unwrap(), 5);
        store.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn budget_charges_are_summed_by_counter_and_admission_time() {
        let dir = scratch("charges");
        let store = Store::open_embedded(&dir).unwrap();
        store
            .append_ledger(vec![
                row("r1", 0, 10, &charged("c", "100")),
                row("r2", 0, 20, &charged("c", "-30")),
                row("r3", 0, 30, &charged("c", "7")),
                row("r4", 0, 30, "not json"),
            ])
            .await
            .unwrap();
        store
            .append_ledger(vec![row("r1", 0, 10, &charged("c", "999"))])
            .await
            .unwrap();
        assert_eq!(store.ledger_charged("c", 0, None).await.unwrap(), 77);
        assert_eq!(store.ledger_charged("c", 15, Some(30)).await.unwrap(), -30);
        assert_eq!(store.ledger_charged("other", 0, None).await.unwrap(), 15);
        assert_eq!(store.ledger_charged("none", 0, None).await.unwrap(), 0);
        store.close();
        let reopened = Store::open_embedded(&dir).unwrap();
        assert_eq!(reopened.ledger_charged("c", 0, None).await.unwrap(), 77);
        reopened.clear_ledger().await.unwrap();
        assert_eq!(reopened.ledger_charged("c", 0, None).await.unwrap(), 0);
        reopened.close();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_lines_without_budgets_skip_the_charges() {
        for body in [
            r#"{"a":1}"#,
            r#"{"budgets":[],"x":{"budgets":[1]}}"#,
            "[1]",
            "\"budgets\"",
        ] {
            assert!(!may_charge(body), "{body}");
        }
        for body in [
            &charged("c", "1"),
            r#"{"budgets":[ ]}"#,
            r#"{"budgets":{"counter":"c"}}"#,
            "not json",
            r#"{"budgets":[],"budgets":[{"counter":"c"}]}"#,
        ] {
            assert!(may_charge(body), "{body}");
        }
    }

    #[tokio::test]
    async fn charges_of_lines_stored_before_the_index_are_backfilled() {
        let dir = scratch("backfill");
        std::fs::create_dir_all(&dir).unwrap();
        {
            let c = rusqlite::Connection::open(dir.join("state.db")).unwrap();
            c.execute_batch(MIGRATIONS[0]).unwrap();
            c.execute(
                "INSERT INTO ledger_lines VALUES ('r', 0, 5, ?1)",
                params![charged("c", "12")],
            )
            .unwrap();
        }
        let store = Store::open_embedded(&dir).unwrap();
        assert_eq!(store.ledger_charged("c", 0, None).await.unwrap(), 12);
        store.close();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

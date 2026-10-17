//! The documents of recent configuration generations, kept beside the generation history so a
//! generation can be read back and rolled back to. Documents are stored as the configuration is
//! (literal secrets sealed), with the kind of actor that made them; only the newest
//! `history_limit` are kept.

use crate::budgets::{Arg, Col};
use crate::{GenerationRecord, Store, StoreError};

pub(crate) const MIGRATIONS: &[&str] = &["CREATE TABLE IF NOT EXISTS generation_documents (
        generation INTEGER PRIMARY KEY,
        document TEXT NOT NULL,
        actor_type TEXT NOT NULL)"];

pub(crate) const PG_MIGRATION: &str = "CREATE TABLE IF NOT EXISTS generation_documents (
        generation bigint PRIMARY KEY,
        document text NOT NULL,
        actor_type text NOT NULL);";

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

impl Store {
    /// Keeps a generation's document and drops those older than the newest `keep`.
    pub async fn put_generation_document(
        &self,
        generation: u64,
        document: String,
        actor_type: String,
        keep: u64,
    ) -> Result<(), StoreError> {
        let g = to_i64(generation);
        self.run_statement(
            "INSERT INTO generation_documents (generation, document, actor_type) VALUES ($1, $2, $3)
             ON CONFLICT (generation) DO UPDATE SET document = excluded.document, actor_type = excluded.actor_type",
            vec![g.into(), document.into(), actor_type.into()],
        )
        .await?;
        let oldest = g.saturating_sub(to_i64(keep)).saturating_add(1);
        self.run_statement(
            "DELETE FROM generation_documents WHERE generation < $1",
            vec![oldest.into()],
        )
        .await
        .map(|_| ())
    }

    /// A kept generation's document and the kind of its actor.
    pub async fn generation_document(&self, generation: u64) -> Result<Option<(String, String)>, StoreError> {
        let rows = self
            .query_cells(
                "SELECT document, actor_type FROM generation_documents WHERE generation = $1",
                vec![to_i64(generation).into()],
                &[Col::Text, Col::Text],
            )
            .await?;
        Ok(rows.into_iter().next().map(|mut c| (c.text(), c.text())))
    }

    /// The generation history, newest first, with each generation's actor kind when its document
    /// is kept: at most `limit` generations older than `before`, none older than `oldest`.
    pub async fn generations(
        &self,
        before: Option<u64>,
        oldest: u64,
        limit: u64,
    ) -> Result<Vec<(GenerationRecord, Option<String>)>, StoreError> {
        let mut sql = String::from(
            "SELECT g.generation, g.created_at, g.actor, g.source, g.summary, d.actor_type
             FROM generations g LEFT JOIN generation_documents d ON d.generation = g.generation
             WHERE g.generation >= $2",
        );
        let mut args: Vec<Arg> = vec![to_i64(limit).into(), to_i64(oldest).into()];
        if let Some(b) = before {
            sql.push_str(" AND g.generation < $3");
            args.push(to_i64(b).into());
        }
        sql.push_str(" ORDER BY g.generation DESC LIMIT $1");
        let rows = self
            .query_cells(
                &sql,
                args,
                &[Col::Int, Col::Text, Col::Text, Col::Text, Col::Text, Col::Text],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|mut c| {
                let record = GenerationRecord {
                    generation: u64::try_from(c.int()).unwrap_or(0),
                    created_at: c.text(),
                    actor: c.text(),
                    source: c.text(),
                    summary: c.text(),
                };
                (record, c.opt_text())
            })
            .collect())
    }
}

//! The PostgreSQL schema: numbered migrations, each run once under an advisory lock and recorded in
//! `schema_migrations` with the oldest binary that may serve the schema it leaves. Migrations only
//! add (expand); nothing a running older node reads is removed in the same release.

use std::time::{Duration, Instant};

use tokio_postgres::Client;

use crate::StoreError;

/// The advisory lock every migrating node takes.
const LOCK_KEY: i64 = 0x616e_7469_7269_6f6e;

/// `(version, name, oldest binary, statements)`.
const MIGRATIONS: &[(i32, &str, &str, &str)] = &[
    (
        1,
        "config_audit_and_access",
        "0.1.0",
        "CREATE TABLE kv (key text PRIMARY KEY, value bytea NOT NULL);
         CREATE TABLE config (
            id integer PRIMARY KEY CHECK (id = 1),
            generation bigint NOT NULL,
            document text NOT NULL,
            metadata text NOT NULL);
         CREATE TABLE generations (
            generation bigint PRIMARY KEY,
            created_at text NOT NULL,
            actor text NOT NULL,
            source text NOT NULL,
            summary text NOT NULL);
         CREATE TABLE audit (
            seq bigserial PRIMARY KEY,
            id text NOT NULL UNIQUE,
            time text NOT NULL,
            time_us bigint NOT NULL,
            actor_id text,
            action text NOT NULL,
            resource text NOT NULL,
            outcome text NOT NULL,
            entry text NOT NULL);
         CREATE INDEX audit_time ON audit (time_us);
         CREATE TABLE admin_tokens (
            kid text PRIMARY KEY,
            user_name text NOT NULL,
            verifier text NOT NULL,
            description text,
            created_at text NOT NULL,
            expires_at text);
         CREATE INDEX admin_tokens_user ON admin_tokens (user_name);
         CREATE TABLE admin_sessions (
            id_digest text PRIMARY KEY,
            kid text NOT NULL,
            created_us bigint NOT NULL,
            last_used_us bigint NOT NULL);
         CREATE TABLE token_bindings (
            id text PRIMARY KEY,
            kid text NOT NULL,
            role text NOT NULL,
            scope text NOT NULL,
            created_at text NOT NULL);
         CREATE INDEX token_bindings_kid ON token_bindings (kid);",
    ),
    (
        2,
        "ledger_and_request_logs",
        "0.1.0",
        "CREATE TABLE ledger_lines (
            request_id text NOT NULL,
            line integer NOT NULL,
            admitted_us bigint NOT NULL,
            body text NOT NULL,
            node_id text NOT NULL,
            inserted_at timestamptz NOT NULL DEFAULT clock_timestamp(),
            PRIMARY KEY (request_id, line));
         CREATE INDEX ledger_lines_inserted ON ledger_lines (inserted_at);
         CREATE TABLE request_logs (
            id text PRIMARY KEY,
            started_us bigint NOT NULL,
            node_id text NOT NULL,
            record text NOT NULL,
            inserted_at timestamptz NOT NULL DEFAULT clock_timestamp());
         CREATE INDEX request_logs_inserted ON request_logs (inserted_at);",
    ),
    (
        3,
        "cluster_state",
        "0.1.0",
        "CREATE TABLE nodes (
            node_id text PRIMARY KEY,
            version text NOT NULL,
            config_generation bigint NOT NULL,
            ready boolean NOT NULL,
            draining boolean NOT NULL,
            drain_requested_at timestamptz,
            degraded text NOT NULL,
            started_at timestamptz NOT NULL,
            last_seen timestamptz NOT NULL);
         CREATE TABLE job_leases (
            job text PRIMARY KEY,
            holder text NOT NULL,
            until timestamptz NOT NULL);
         CREATE TABLE cache_epochs (
            k text PRIMARY KEY,
            epoch bigint NOT NULL);
         CREATE TABLE response_links (
            id text PRIMARY KEY,
            project text NOT NULL,
            deployment text NOT NULL,
            created_us bigint NOT NULL);
         CREATE TABLE stored_responses (
            id text PRIMARY KEY,
            project text NOT NULL,
            body text NOT NULL,
            created_us bigint NOT NULL);
         CREATE INDEX stored_responses_created ON stored_responses (created_us);
         CREATE TABLE oidc_logins (
            state text PRIMARY KEY,
            body text NOT NULL,
            created_us bigint NOT NULL);",
    ),
    (
        4,
        "ledger_charges",
        "0.1.0",
        "CREATE TABLE ledger_charges (
            counter text NOT NULL,
            admitted_us bigint NOT NULL,
            request_id text NOT NULL,
            line integer NOT NULL,
            amount numeric NOT NULL,
            PRIMARY KEY (counter, admitted_us, request_id, line));
         CREATE INDEX ledger_lines_admitted ON ledger_lines (admitted_us);
         INSERT INTO ledger_charges (counter, admitted_us, request_id, line, amount)
            SELECT b->>'counter', l.admitted_us, l.request_id, l.line, (b->>'amount')::numeric
            FROM ledger_lines l,
                 jsonb_array_elements(CASE WHEN jsonb_typeof(l.body::jsonb->'budgets') = 'array'
                                           THEN l.body::jsonb->'budgets' ELSE '[]'::jsonb END) b
            WHERE b ? 'counter' AND b ? 'amount'
            ON CONFLICT DO NOTHING;",
    ),
    (
        5,
        "console_identity",
        "0.1.0",
        "ALTER TABLE admin_sessions ADD COLUMN IF NOT EXISTS user_name text;
         ALTER TABLE admin_sessions ADD COLUMN IF NOT EXISTS facts text NOT NULL DEFAULT '{}';
         CREATE INDEX IF NOT EXISTS admin_sessions_user ON admin_sessions (user_name);
         CREATE INDEX IF NOT EXISTS admin_sessions_kid ON admin_sessions (kid);
         ALTER TABLE admin_tokens ADD COLUMN IF NOT EXISTS access text;
         ALTER TABLE admin_tokens ADD COLUMN IF NOT EXISTS last_used_us bigint;
         CREATE TABLE identity_records (
            kind text NOT NULL,
            id text NOT NULL,
            user_name text NOT NULL,
            expires_us bigint,
            body text NOT NULL,
            PRIMARY KEY (kind, id));
         CREATE INDEX identity_records_user ON identity_records (user_name, kind);",
    ),
    (
        6,
        "console_analytics",
        "0.1.0",
        "ALTER TABLE nodes ADD COLUMN IF NOT EXISTS in_flight bigint NOT NULL DEFAULT 0;
         ALTER TABLE nodes ADD COLUMN IF NOT EXISTS max_in_flight bigint;
         ALTER TABLE nodes ADD COLUMN IF NOT EXISTS queue_depth bigint NOT NULL DEFAULT 0;
         ALTER TABLE nodes ADD COLUMN IF NOT EXISTS started_us bigint NOT NULL DEFAULT 0;
         ALTER TABLE nodes ADD COLUMN IF NOT EXISTS ready_since_us bigint;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS request_id text NOT NULL DEFAULT '';
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS organization text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS team text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS project text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS key_name text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS user_name text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS model text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS requested_model text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS provider text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS deployment text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS route text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS route_node text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS status_code bigint NOT NULL DEFAULT 0;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS code text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS response_path text NOT NULL DEFAULT '';
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS operation text NOT NULL DEFAULT '';
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS end_user text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS session_id text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS environment text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS tags text NOT NULL DEFAULT '';
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS guardrails text NOT NULL DEFAULT '';
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS verdicts text NOT NULL DEFAULT '';
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS categories text NOT NULL DEFAULT '';
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS flagged bigint NOT NULL DEFAULT 0;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS stream bigint NOT NULL DEFAULT 0;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS cache_status text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS latency_ms double precision NOT NULL DEFAULT 0;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS ttft_ms double precision NOT NULL DEFAULT -1;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS cost_nusd bigint NOT NULL DEFAULT -1;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS input_tokens bigint NOT NULL DEFAULT 0;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS output_tokens bigint NOT NULL DEFAULT 0;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS client_ip text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS content text;
         ALTER TABLE request_logs ADD COLUMN IF NOT EXISTS prompt text;
         CREATE INDEX IF NOT EXISTS request_logs_started ON request_logs (started_us, id);
         CREATE INDEX IF NOT EXISTS request_logs_key ON request_logs (key_name, started_us);
         CREATE INDEX IF NOT EXISTS request_logs_project ON request_logs (project, started_us);
         CREATE INDEX IF NOT EXISTS request_logs_team ON request_logs (team, started_us);
         CREATE INDEX IF NOT EXISTS request_logs_org ON request_logs (organization, started_us);
         CREATE INDEX IF NOT EXISTS request_logs_model ON request_logs (model, started_us);
         CREATE INDEX IF NOT EXISTS request_logs_user ON request_logs (user_name, started_us);
         CREATE INDEX IF NOT EXISTS request_logs_request ON request_logs (request_id);
         CREATE INDEX IF NOT EXISTS request_logs_end_user ON request_logs (end_user, started_us);
         CREATE TABLE log_rollups (
            grain_s bigint NOT NULL, start_us bigint NOT NULL, node_id text NOT NULL, dims_key text NOT NULL,
            organization text, team text, project text, key_name text, user_name text, model text, requested_model text, provider text, deployment text, route text, route_node text, status_code text, code text, response_path text, cache_status text, environment text, stream text,
            requests bigint NOT NULL, errors bigint NOT NULL, cancelled bigint NOT NULL, fallbacks bigint NOT NULL, retries bigint NOT NULL, cache_hits bigint NOT NULL, guardrail_blocks bigint NOT NULL, guardrail_redactions bigint NOT NULL, guardrail_flags bigint NOT NULL, input bigint NOT NULL, input_cache_read bigint NOT NULL, input_cache_write bigint NOT NULL, output bigint NOT NULL, output_reasoning bigint NOT NULL, cost_nusd bigint NOT NULL, cache_saved_nusd bigint NOT NULL,
            latency bytea NOT NULL, ttft bytea NOT NULL, otps bytea NOT NULL,
            PRIMARY KEY (grain_s, start_us, node_id, dims_key));
         CREATE INDEX log_rollups_start ON log_rollups (grain_s, start_us);",
    ),
    (
        7,
        "alerting_and_delivery",
        "0.1.0",
        crate::alerting::PG_MIGRATION,
    ),
    (25, "upstream_objects", "0.1.0", crate::objects::PG_MIGRATION),
    (
        30,
        "log_feedback",
        "0.1.0",
        crate::logs::feedback::PG_MIGRATION,
    ),
    (35, "pending_requests", "0.1.0", crate::requests::PG_MIGRATION),
    (45, "use_marks", "0.1.0", crate::marks::PG_MIGRATION),
    (46, "key_anomalies", "0.1.0", crate::anomalies::PG_MIGRATION),
    (50, "budget_console", "0.1.0", crate::budgets::PG_MIGRATION),
    (51, "generation_documents", "0.1.0", crate::generations::PG_MIGRATION),
    (55, "safety_events", "0.1.0", crate::safety::PG_MIGRATION),
    (60, "cache_console", "0.1.0", crate::cache_console::PG_MIGRATION),
    (70, "bandit_rewards", "0.1.0", crate::bandits::PG_MIGRATION),
    (85, "provider_pressure", "0.1.0", crate::pressure::PG_MIGRATION),
    (
        86,
        "rollup_cache_prefix",
        "0.1.0",
        "ALTER TABLE log_rollups ADD COLUMN IF NOT EXISTS cache_prefix text;",
    ),
];

/// `a < b` for dotted numeric versions; parts that are not numbers compare as 0.
fn older(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split(['.', '-', '+'])
            .take(3)
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parts(a) < parts(b)
}

/// Checks that `binary` may serve the recorded schema.
async fn check_compatible(c: &Client, binary: &str) -> Result<(), StoreError> {
    let rows = c
        .query(
            "SELECT version, min_binary FROM schema_migrations ORDER BY version",
            &[],
        )
        .await?;
    for row in rows {
        let version: i32 = row.try_get(0)?;
        let min: String = row.try_get(1)?;
        if older(binary, &min) {
            return Err(StoreError::SchemaTooNew {
                schema: version,
                needs: min,
                binary: binary.to_owned(),
            });
        }
    }
    Ok(())
}

async fn applied(c: &Client) -> Result<Vec<i32>, StoreError> {
    let rows = c.query("SELECT version FROM schema_migrations", &[]).await?;
    rows.iter()
        .map(|r| r.try_get::<_, i32>(0).map_err(StoreError::from))
        .collect()
}

/// Runs the pending migrations under the advisory lock, waiting up to `timeout` for a node that
/// holds it. Returns how many this node ran.
pub(crate) async fn migrate(c: &mut Client, binary: &str, timeout: Duration) -> Result<usize, StoreError> {
    let deadline = Instant::now() + timeout;
    loop {
        let row = c
            .query_one("SELECT pg_try_advisory_lock($1)", &[&LOCK_KEY])
            .await?;
        if row.try_get::<_, bool>(0)? {
            break;
        }
        if Instant::now() >= deadline {
            return Err(StoreError::Timeout);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let result = run_pending(c, binary).await;
    let unlock = c.query_one("SELECT pg_advisory_unlock($1)", &[&LOCK_KEY]).await;
    let ran = result?;
    unlock?;
    Ok(ran)
}

async fn run_pending(c: &mut Client, binary: &str) -> Result<usize, StoreError> {
    c.batch_execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version integer PRIMARY KEY,
            name text NOT NULL,
            min_binary text NOT NULL,
            applied_at timestamptz NOT NULL DEFAULT now())",
    )
    .await?;
    check_compatible(c, binary).await?;
    let done = applied(c).await?;
    let mut ran = 0;
    for (version, name, min, sql) in MIGRATIONS {
        if done.contains(version) {
            continue;
        }
        let tx = c.transaction().await?;
        tx.batch_execute(sql).await?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, min_binary) VALUES ($1, $2, $3)",
            &[version, name, min],
        )
        .await?;
        tx.commit().await?;
        tracing::info!(version, name, "store migration applied");
        ran += 1;
    }
    Ok(ran)
}

/// With `migrate: manual`: the schema must already be complete and compatible.
pub(crate) async fn verify(c: &Client, binary: &str) -> Result<(), StoreError> {
    check_compatible(c, binary).await?;
    let done = applied(c).await?;
    match MIGRATIONS.iter().find(|(v, ..)| !done.contains(v)) {
        Some((v, name, ..)) => Err(StoreError::NotMigrated(format!(
            "migration {v} ({name}) has not run"
        ))),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(older("0.1.0", "0.10.0"));
        assert!(older("0.9.9", "1.0.0"));
        assert!(!older("1.0.0", "1.0.0"));
        assert!(!older("1.2.0-rc1", "1.1.9"));
    }

    #[test]
    fn the_log_migration_creates_every_rollup_column() {
        let sql: String = MIGRATIONS
            .iter()
            .filter(|m| matches!(m.1, "console_analytics" | "rollup_cache_prefix"))
            .map(|m| m.3)
            .collect();
        for c in crate::logs::ROLLUP_DIMS
            .iter()
            .chain(crate::logs::MEASURES.iter())
            .chain(crate::logs::SKETCHES.iter())
        {
            assert!(sql.contains(&format!(" {c} ")), "{c}");
        }
    }

    #[test]
    fn migrations_are_numbered_in_order() {
        let versions: Vec<i32> = MIGRATIONS.iter().map(|m| m.0).collect();
        let mut sorted = versions.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(versions, sorted);
    }
}

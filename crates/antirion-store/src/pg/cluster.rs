//! State only a cluster shares through the store: node rows and drain commands, job leases,
//! request-log records, Responses state, single sign-on logins and cache epochs.

use std::time::Duration;

use super::PgStore;
use crate::StoreError;

/// One node as the store last saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeRow {
    pub node_id: String,
    pub version: String,
    pub config_generation: u64,
    pub ready: bool,
    pub draining: bool,
    pub degraded: Vec<String>,
    pub last_seen_us: i64,
    /// Seen within the node time-to-live, by the store's clock.
    pub alive: bool,
    pub load: NodeLoad,
}

/// What a node reports of its load and life.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeLoad {
    /// Data-plane requests in progress.
    pub in_flight: i64,
    /// The admission cap, when one is set.
    pub max_in_flight: Option<i64>,
    pub queue_depth: i64,
    pub started_us: i64,
    pub ready_since_us: Option<i64>,
}

/// A background job's lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobLease {
    pub job: String,
    pub holder: String,
    pub expires_us: i64,
}

/// A stored Responses object or a native link, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedResponse {
    pub project: String,
    /// The stored body, or the deployment of a native link.
    pub value: String,
    pub created_us: i64,
}

const US: &str = "floor(extract(epoch FROM {}) * 1000000)::int8";

fn us(column: &str) -> String {
    US.replace("{}", column)
}

fn millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

impl PgStore {
    /// Records this node's state; true when another node asked it to drain.
    pub async fn heartbeat(&self, row: &NodeRow) -> Result<bool, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let generation = i64::try_from(row.config_generation).unwrap_or(i64::MAX);
            let degraded = row.degraded.join(",");
            let r = lease
                .client()?
                .query_one(
                    "INSERT INTO nodes (node_id, version, config_generation, ready, draining, degraded,
                         started_at, last_seen, in_flight, max_in_flight, queue_depth, started_us, ready_since_us)
                     VALUES ($1, $2, $3, $4, $5, $6, clock_timestamp(), clock_timestamp(), $7, $8, $9, $10, $11)
                     ON CONFLICT (node_id) DO UPDATE SET version = excluded.version,
                         config_generation = excluded.config_generation, ready = excluded.ready,
                         draining = excluded.draining, degraded = excluded.degraded,
                         last_seen = excluded.last_seen, in_flight = excluded.in_flight,
                         max_in_flight = excluded.max_in_flight, queue_depth = excluded.queue_depth,
                         started_us = excluded.started_us, ready_since_us = excluded.ready_since_us
                     RETURNING drain_requested_at IS NOT NULL",
                    &[
                        &row.node_id,
                        &row.version,
                        &generation,
                        &row.ready,
                        &row.draining,
                        &degraded,
                        &row.load.in_flight,
                        &row.load.max_in_flight,
                        &row.load.queue_depth,
                        &row.load.started_us,
                        &row.load.ready_since_us,
                    ],
                )
                .await?;
            Ok(r.try_get(0)?)
        })
        .await
    }

    /// Clears a drain command when a node starts again under the same id.
    pub async fn clear_drain(&self, node_id: &str) -> Result<(), StoreError> {
        self.execute(
            "UPDATE nodes SET drain_requested_at = NULL WHERE node_id = $1",
            &[&node_id],
        )
        .await
        .map(|_| ())
    }

    /// Every node row, newest heartbeat first.
    pub async fn nodes(&self, ttl: Duration) -> Result<Vec<NodeRow>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let sql = format!(
                "SELECT node_id, version, config_generation, ready, draining, degraded, {},
                     last_seen > clock_timestamp() - make_interval(secs => $1::int8::float8 / 1000.0),
                     in_flight, max_in_flight, queue_depth, started_us, ready_since_us
                 FROM nodes ORDER BY node_id",
                us("last_seen")
            );
            let rows = lease.client()?.query(sql.as_str(), &[&millis(ttl)]).await?;
            rows.iter()
                .map(|r| {
                    let degraded: String = r.try_get(5)?;
                    Ok(NodeRow {
                        node_id: r.try_get(0)?,
                        version: r.try_get(1)?,
                        config_generation: u64::try_from(r.try_get::<_, i64>(2)?).unwrap_or(0),
                        ready: r.try_get(3)?,
                        draining: r.try_get(4)?,
                        degraded: degraded
                            .split(',')
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .collect(),
                        last_seen_us: r.try_get(6)?,
                        alive: r.try_get(7)?,
                        load: NodeLoad {
                            in_flight: r.try_get(8)?,
                            max_in_flight: r.try_get(9)?,
                            queue_depth: r.try_get(10)?,
                            started_us: r.try_get(11)?,
                            ready_since_us: r.try_get(12)?,
                        },
                    })
                })
                .collect()
        })
        .await
    }

    /// Asks node `node_id` to drain; false when no such node is known.
    pub async fn request_drain(&self, node_id: &str) -> Result<bool, StoreError> {
        self.execute(
            "UPDATE nodes SET drain_requested_at = clock_timestamp() WHERE node_id = $1",
            &[&node_id],
        )
        .await
        .map(|n| n > 0)
    }

    /// Forgets nodes not seen for `older_than`.
    pub async fn forget_nodes(&self, older_than: Duration) -> Result<u64, StoreError> {
        self.execute(
            "DELETE FROM nodes WHERE last_seen < clock_timestamp() - make_interval(secs => $1::int8::float8 / 1000.0)",
            &[&millis(older_than)],
        )
        .await
    }

    /// Takes or renews the lease on `job` for `holder`; true when `holder` holds it afterwards.
    pub async fn lease(&self, job: &str, holder: &str, ttl: Duration) -> Result<bool, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let row = lease
                .client()?
                .query_opt(
                    "INSERT INTO job_leases (job, holder, until)
                     VALUES ($1, $2, clock_timestamp() + make_interval(secs => $3::int8::float8 / 1000.0))
                     ON CONFLICT (job) DO UPDATE SET holder = excluded.holder, until = excluded.until
                     WHERE job_leases.holder = excluded.holder OR job_leases.until < clock_timestamp()
                     RETURNING holder",
                    &[&job, &holder, &millis(ttl)],
                )
                .await?;
            Ok(row.is_some())
        })
        .await
    }

    pub async fn leases(&self) -> Result<Vec<JobLease>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let sql = format!("SELECT job, holder, {} FROM job_leases ORDER BY job", us("until"));
            let rows = lease.client()?.query(sql.as_str(), &[]).await?;
            rows.iter()
                .map(|r| {
                    Ok(JobLease {
                        job: r.try_get(0)?,
                        holder: r.try_get(1)?,
                        expires_us: r.try_get(2)?,
                    })
                })
                .collect()
        })
        .await
    }

    pub async fn put_response_link(&self, id: &str, r: &SharedResponse) -> Result<(), StoreError> {
        self.execute(
            "INSERT INTO response_links (id, project, deployment, created_us) VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET project = excluded.project, deployment = excluded.deployment,
                 created_us = excluded.created_us",
            &[&id, &r.project, &r.value, &r.created_us],
        )
        .await
        .map(|_| ())
    }

    pub async fn response_link(&self, id: &str) -> Result<Option<SharedResponse>, StoreError> {
        self.shared_row(
            "SELECT project, deployment, created_us FROM response_links WHERE id = $1",
            id,
        )
        .await
    }

    pub async fn put_response(&self, id: &str, r: &SharedResponse) -> Result<(), StoreError> {
        self.execute(
            "INSERT INTO stored_responses (id, project, body, created_us) VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET project = excluded.project, body = excluded.body,
                 created_us = excluded.created_us",
            &[&id, &r.project, &r.value, &r.created_us],
        )
        .await
        .map(|_| ())
    }

    pub async fn response(&self, id: &str) -> Result<Option<SharedResponse>, StoreError> {
        self.shared_row(
            "SELECT project, body, created_us FROM stored_responses WHERE id = $1",
            id,
        )
        .await
    }

    /// Deletes a stored response of `project`; whether there was one.
    pub async fn delete_response(&self, id: &str, project: &str) -> Result<bool, StoreError> {
        self.execute(
            "DELETE FROM stored_responses WHERE id = $1 AND project = $2",
            &[&id, &project],
        )
        .await
        .map(|n| n > 0)
    }

    /// Removes Responses state and logins created before `before_us`.
    pub async fn purge_responses(&self, before_us: i64, logins_before_us: i64) -> Result<(), StoreError> {
        self.execute(
            "DELETE FROM stored_responses WHERE created_us < $1",
            &[&before_us],
        )
        .await?;
        self.execute("DELETE FROM response_links WHERE created_us < $1", &[&before_us])
            .await?;
        self.execute(
            "DELETE FROM oidc_logins WHERE created_us < $1",
            &[&logins_before_us],
        )
        .await?;
        Ok(())
    }

    async fn shared_row(&self, sql: &str, id: &str) -> Result<Option<SharedResponse>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let row = lease.client()?.query_opt(sql, &[&id]).await?;
            row.map(|r| {
                Ok(SharedResponse {
                    project: r.try_get(0)?,
                    value: r.try_get(1)?,
                    created_us: r.try_get(2)?,
                })
            })
            .transpose()
        })
        .await
    }

    pub async fn put_login(&self, state: &str, body: &str, created_us: i64) -> Result<(), StoreError> {
        self.execute(
            "INSERT INTO oidc_logins (state, body, created_us) VALUES ($1, $2, $3)
             ON CONFLICT (state) DO UPDATE SET body = excluded.body, created_us = excluded.created_us",
            &[&state, &body, &created_us],
        )
        .await
        .map(|_| ())
    }

    /// A login in progress, left in place.
    pub async fn login(&self, state: &str) -> Result<Option<String>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let row = lease
                .client()?
                .query_opt("SELECT body FROM oidc_logins WHERE state = $1", &[&state])
                .await?;
            Ok(row.map(|r| r.try_get(0)).transpose()?)
        })
        .await
    }

    /// Removes and returns a login in progress.
    pub async fn take_login(&self, state: &str) -> Result<Option<String>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let row = lease
                .client()?
                .query_opt(
                    "DELETE FROM oidc_logins WHERE state = $1 RETURNING body",
                    &[&state],
                )
                .await?;
            Ok(row.map(|r| r.try_get(0)).transpose()?)
        })
        .await
    }

    /// Raises each epoch to at least the given value.
    pub async fn raise_epochs(&self, epochs: &[(String, i64)]) -> Result<(), StoreError> {
        if epochs.is_empty() {
            return Ok(());
        }
        let keys: Vec<&str> = epochs.iter().map(|(k, _)| k.as_str()).collect();
        let values: Vec<i64> = epochs.iter().map(|(_, v)| *v).collect();
        self.execute(
            "INSERT INTO cache_epochs (k, epoch) SELECT * FROM unnest($1::text[], $2::int8[])
             ON CONFLICT (k) DO UPDATE SET epoch = GREATEST(cache_epochs.epoch, excluded.epoch)",
            &[&keys, &values],
        )
        .await
        .map(|_| ())
    }

    pub async fn epochs(&self) -> Result<Vec<(String, i64)>, StoreError> {
        self.run(async {
            let mut lease = self.pool.get().await?;
            let rows = lease
                .client()?
                .query("SELECT k, epoch FROM cache_epochs", &[])
                .await?;
            rows.iter().map(|r| Ok((r.try_get(0)?, r.try_get(1)?))).collect()
        })
        .await
    }

    pub async fn clear_epochs(&self) -> Result<(), StoreError> {
        self.execute("DELETE FROM cache_epochs", &[]).await.map(|_| ())
    }
}

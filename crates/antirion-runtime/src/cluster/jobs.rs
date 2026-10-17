//! Background jobs, each run by at most one node at a time under a lease in the store (15 s,
//! renewed every 5 s). Nodes that do not hold a lease try to take it every second, so a job moves
//! within the lease's lifetime when its holder dies. No request depends on a job running on time.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use super::pause;
use crate::Runtime;

/// The jobs, with how often their holder runs them.
pub const JOBS: &[(&str, Duration)] = &[
    ("counter_reaper", Duration::from_secs(5)),
    ("retention", Duration::from_secs(60)),
];

const LEASE_TTL: Duration = Duration::from_secs(15);
const RENEW_EVERY: Duration = Duration::from_secs(5);
const TRY_EVERY: Duration = Duration::from_secs(1);

/// Logins in progress are kept this long.
const LOGIN_TTL: Duration = Duration::from_secs(600);
/// Node rows not seen for this long are removed.
const FORGET_NODES: Duration = Duration::from_secs(3600);

#[derive(Default)]
struct Held {
    renewed: Option<Instant>,
    last_run: Option<Instant>,
}

pub(crate) async fn run(rt: Arc<Runtime>) {
    let Some(cluster) = &rt.cluster else { return };
    let Some(pg) = rt.store.postgres() else { return };
    let mut held: HashMap<&str, Held> = HashMap::new();
    while pause(&rt, TRY_EVERY).await {
        if pg.is_down() {
            held.clear();
            continue;
        }
        for (job, every) in JOBS {
            let h = held.entry(job).or_default();
            let fresh = h.renewed.is_some_and(|t| t.elapsed() < RENEW_EVERY);
            if !fresh {
                match pg.lease(job, &rt.node.id, LEASE_TTL).await {
                    Ok(true) => h.renewed = Some(Instant::now()),
                    Ok(false) | Err(_) => {
                        h.renewed = None;
                        continue;
                    }
                }
            }
            if h.last_run.is_none_or(|t| t.elapsed() >= *every) {
                h.last_run = Some(Instant::now());
                run_job(&rt, job).await;
            }
        }
        if let Ok(leases) = pg.leases().await {
            cluster.view.lock().leases = leases;
        }
    }
}

async fn run_job(rt: &Runtime, job: &str) {
    let Some(pg) = rt.store.postgres() else { return };
    match job {
        "counter_reaper" => match rt.accounts.engine().reap(100).await {
            Ok(expired) if !expired.is_empty() => {
                tracing::info!(
                    count = expired.len(),
                    "expired the reservations of nodes that stopped"
                );
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(error = %e, "the counter reaper could not run"),
        },
        "retention" => {
            let now = rt.now();
            let ttl = crate::responses::ttl_of(&rt.gens.load().config.doc);
            let before = |d: Duration| now.0 - i64::try_from(d.as_micros()).unwrap_or(i64::MAX);
            let results = [
                pg.purge_responses(before(ttl), before(LOGIN_TTL)).await.err(),
                pg.forget_nodes(FORGET_NODES).await.err(),
            ];
            if let Err(e) = rt.expire_logs().await {
                tracing::debug!(error = %e, "request-log retention failed");
            }
            for e in results.into_iter().flatten() {
                tracing::debug!(error = %e, "a retention step failed");
            }
        }
        _ => {}
    }
}

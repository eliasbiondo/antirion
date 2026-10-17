//! Metrics of self-hosted pools: one task per node scrapes every endpoint of every `pool` node of the
//! serving generation every `metrics.interval`, bounded by `metrics.timeout`, at most one scrape per
//! endpoint at a time, and hands what it read to the routing state. A failed scrape keeps the old
//! reading, which goes stale after three intervals. The metrics URL of an endpoint is its provider's
//! base URL without a final `/v1` segment, followed by `metrics.path`, sent with the provider's TLS
//! profile under the egress rules of operator destinations.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use antirion_egress::{OutboundRequest, TrustClass};
use antirion_providers::DeploymentRt;
use antirion_router::pool::{PoolSpec, metrics};
use antirion_router::tree::{CompiledRoute, NodeKind};
use antirion_router::{RouteRt, RoutingSnapshot};
use bytes::Bytes;
use futures_util::StreamExt;
use http::{HeaderMap, Method, Uri};
use http_body_util::BodyExt;
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::Runtime;

/// How often the task looks for scrapes that are due; the shortest interval allowed.
const TICK: Duration = Duration::from_millis(50);
/// The largest metrics page read.
const MAX_PAGE: usize = 4 << 20;

/// One endpoint of one pool.
#[derive(Clone, Debug)]
struct Target {
    pool: String,
    deployment: String,
    spec: Arc<PoolSpec>,
}

fn pools_of(tree: &CompiledRoute, out: &mut Vec<Target>) {
    for n in &tree.nodes {
        if let Some(m) = &n.mirror {
            pools_of(&m.target, out);
        }
        let NodeKind::Pool(spec) = &n.kind else { continue };
        for c in &n.children {
            if let Some(NodeKind::Deployment(d)) = tree.node(*c).map(|c| &c.kind) {
                out.push(Target {
                    pool: n.state_key.clone(),
                    deployment: d.clone(),
                    spec: Arc::clone(spec),
                });
            }
        }
    }
}

fn targets(routing: &RoutingSnapshot) -> Vec<Target> {
    let mut out = Vec::new();
    let routes: Vec<&Arc<RouteRt>> = routing
        .routes
        .values()
        .chain(routing.model_routes.values())
        .collect();
    for r in routes {
        pools_of(&r.tree, &mut out);
    }
    out
}

/// The metrics URL of an endpoint.
fn metrics_url(base: &str, path: &str) -> Option<Uri> {
    let base = base.trim_end_matches('/');
    let root = base.strip_suffix("/v1").unwrap_or(base);
    format!("{root}{path}").parse().ok()
}

/// Runs the scrapes of every pool until the node stops.
pub async fn run(rt: Arc<Runtime>) {
    let mut running: JoinSet<(
        String,
        String,
        Option<metrics::Reading>,
        std::time::Instant,
        usize,
    )> = JoinSet::new();
    let mut busy: HashSet<(String, String)> = HashSet::new();
    let mut last: HashMap<(String, String), Instant> = HashMap::new();
    loop {
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            Some(done) = running.join_next(), if !running.is_empty() => {
                if let Ok((pool, deployment, reading, started, entries)) = done {
                    rt.state.routing.pools().scraped(&pool, &deployment, reading, started, entries);
                    busy.remove(&(pool, deployment));
                }
                continue;
            }
            () = tokio::time::sleep(TICK) => {}
        }
        let snap = rt.gens.load();
        let all = targets(&snap.routing);
        if all.is_empty() {
            last.clear();
            continue;
        }
        let now = Instant::now();
        for t in all {
            let id = (t.pool.clone(), t.deployment.clone());
            if busy.contains(&id) || last.get(&id).is_some_and(|at| now < *at + t.spec.scrape.interval) {
                continue;
            }
            let Some(d) = snap.providers.deployments.get(&t.deployment).cloned() else {
                continue;
            };
            last.insert(id.clone(), now);
            busy.insert(id);
            let rt = Arc::clone(&rt);
            let policy = Arc::clone(&snap.security.egress);
            running.spawn(async move {
                let started = std::time::Instant::now();
                let timeout = t.spec.scrape.timeout;
                let reading = tokio::time::timeout(timeout, scrape(&rt, &d, &t.spec, policy))
                    .await
                    .ok()
                    .flatten();
                (t.pool, t.deployment, reading, started, t.spec.entries)
            });
        }
    }
}

/// One scrape: the reading, or `None` when the endpoint did not answer with usable metrics.
async fn scrape(
    rt: &Runtime,
    d: &DeploymentRt,
    spec: &PoolSpec,
    policy: Arc<antirion_egress::EgressPolicy>,
) -> Option<metrics::Reading> {
    let uri = metrics_url(d.provider.base_url.as_deref()?, &spec.scrape.path)?;
    let resp = rt
        .egress
        .send(OutboundRequest {
            method: Method::GET,
            uri,
            headers: HeaderMap::new(),
            body: Bytes::new().into(),
            response_timeout: spec.scrape.timeout,
            connect_timeout: Some(spec.scrape.timeout),
            fresh_connection: false,
            trust: TrustClass::Operator,
            tls: d.provider.tls.clone(),
            policy,
            resend_when_closed: false,
        })
        .await
        .ok()?;
    if !resp.status.is_success() {
        return None;
    }
    let mut chunks = resp.body.into_data_stream();
    let mut page = Vec::new();
    while let Some(chunk) = chunks.next().await {
        page.extend_from_slice(&chunk.ok()?);
        if page.len() > MAX_PAGE {
            return None;
        }
    }
    let mut reading = metrics::read(std::str::from_utf8(&page).ok()?, &spec.scrape.names)?;
    reading.served = Some(d.model.clone());
    Some(reading)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_metrics_url_drops_the_api_version() {
        let u = |b: &str| metrics_url(b, "/metrics").map(|u| u.to_string());
        assert_eq!(
            u("http://10.0.0.5:8000/v1").as_deref(),
            Some("http://10.0.0.5:8000/metrics")
        );
        assert_eq!(u("http://h:8000/v1/").as_deref(), Some("http://h:8000/metrics"));
        assert_eq!(
            u("http://mock/inst/v1").as_deref(),
            Some("http://mock/inst/metrics")
        );
        assert_eq!(u("http://h:8000").as_deref(), Some("http://h:8000/metrics"));
    }
}

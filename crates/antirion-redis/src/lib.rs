//! Connections to a Redis-protocol server (Valkey or Redis): one server through a reconnecting
//! manager, or a Redis Cluster through a cluster client that follows `MOVED` and `ASK`
//! redirections and retries `TRYAGAIN`. A redirection the cluster client hands back while a slot
//! moves (a pipeline's `MOVED`, a node it has not connected to yet) is retried here a few times, so
//! a reshard surfaces no error. Both connect on first use, so a node starts while the server is
//! down.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

use std::sync::Arc;
use std::time::Duration;

use redis::aio::{ConnectionLike, ConnectionManager, ConnectionManagerConfig};
use redis::cluster::ClusterClient;
use redis::cluster_async::ClusterConnection;
use redis::cluster_routing::{MultipleNodeRoutingInfo, RoutingInfo, SingleNodeRoutingInfo};
use redis::{Cmd, ErrorKind, Pipeline, RedisError, RedisFuture, RedisResult, Value};
use tokio::sync::OnceCell;

/// How the servers are deployed (`cluster.counters.redis.topology`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Topology {
    /// One server, or the primary a Sentinel setup names.
    #[default]
    Standalone,
    /// A Redis Cluster: keys spread over hash slots, each owned by one primary.
    Cluster,
}

impl Topology {
    /// The configuration value: `standalone`, `sentinel` or `cluster`.
    #[must_use]
    pub fn parse(s: &str) -> Topology {
        if s == "cluster" {
            Topology::Cluster
        } else {
            Topology::Standalone
        }
    }

    /// `prefix` made a hash tag on a cluster, so every key under it falls in one slot and the
    /// multi-key operations of a store stay atomic.
    #[must_use]
    pub fn slot_prefix(self, prefix: &str) -> String {
        match self {
            Topology::Standalone => prefix.to_owned(),
            Topology::Cluster => format!("{{{prefix}}}"),
        }
    }
}

struct LazyCluster {
    client: ClusterClient,
    conn: OnceCell<ClusterConnection>,
}

impl LazyCluster {
    async fn get(&self) -> RedisResult<ClusterConnection> {
        self.conn
            .get_or_try_init(|| self.client.get_async_connection())
            .await
            .cloned()
    }
}

#[derive(Clone)]
enum Inner {
    Single(ConnectionManager),
    Cluster(Arc<LazyCluster>),
}

/// A connection to a server or a cluster; clones share it.
#[derive(Clone)]
pub struct RedisConn {
    inner: Inner,
}

impl std::fmt::Debug for RedisConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.inner {
            Inner::Single(_) => "standalone",
            Inner::Cluster(_) => "cluster",
        };
        f.debug_struct("RedisConn").field("topology", &kind).finish()
    }
}

impl RedisConn {
    /// A connection to `url` (`redis://…` or `rediss://…`); on a cluster, `url` is one of its nodes
    /// and the others are discovered. A standalone connection retries a failed command `retries`
    /// times on a new connection.
    pub fn open(url: &str, topology: Topology, retries: usize) -> RedisResult<RedisConn> {
        let inner = match topology {
            Topology::Standalone => {
                let client = redis::Client::open(url)?;
                let config = ConnectionManagerConfig::new().set_number_of_retries(retries);
                Inner::Single(client.get_connection_manager_lazy(config)?)
            }
            Topology::Cluster => Inner::Cluster(Arc::new(LazyCluster {
                client: ClusterClient::builder([url]).retries(3).build()?,
                conn: OnceCell::new(),
            })),
        };
        Ok(RedisConn { inner })
    }

    #[must_use]
    pub fn topology(&self) -> Topology {
        match self.inner {
            Inner::Single(_) => Topology::Standalone,
            Inner::Cluster(_) => Topology::Cluster,
        }
    }

    /// The keys matching `pattern`, from every primary of a cluster.
    pub async fn scan(&self, pattern: &str) -> RedisResult<Vec<String>> {
        match &self.inner {
            Inner::Single(c) => scan_on(&mut c.clone(), pattern).await,
            Inner::Cluster(l) => {
                let mut conn = l.get().await?;
                let first = scan_cmd(0, pattern);
                let all = RoutingInfo::MultiNode((MultipleNodeRoutingInfo::AllMasters, None));
                let replies = conn.route_command(first, all).await?;
                let Value::Map(per_node) = replies else {
                    return Err(protocol("a cluster SCAN did not answer per node"));
                };
                let mut keys = Vec::new();
                for (addr, reply) in per_node {
                    let addr: String = redis::from_redis_value(addr)?;
                    let (cursor, found): (u64, Vec<String>) = redis::from_redis_value(reply)?;
                    keys.extend(found);
                    if cursor != 0 {
                        let (host, port) = addr
                            .rsplit_once(':')
                            .and_then(|(h, p)| Some((h.to_owned(), p.parse::<u16>().ok()?)))
                            .ok_or_else(|| protocol("a cluster node address is not host:port"))?;
                        let node = SingleNodeRoutingInfo::ByAddress { host, port };
                        keys.extend(scan_routed(&mut conn, &node, cursor, pattern).await?);
                    }
                }
                Ok(keys)
            }
        }
    }
}

fn protocol(what: &'static str) -> RedisError {
    RedisError::from((redis::ErrorKind::Parse, what))
}

fn scan_cmd(cursor: u64, pattern: &str) -> Cmd {
    let mut cmd = redis::cmd("SCAN");
    cmd.arg(cursor).arg("MATCH").arg(pattern).arg("COUNT").arg(1000);
    cmd
}

async fn scan_on(conn: &mut ConnectionManager, pattern: &str) -> RedisResult<Vec<String>> {
    let mut cursor = 0;
    let mut found = Vec::new();
    loop {
        let (next, keys): (u64, Vec<String>) = scan_cmd(cursor, pattern).query_async(conn).await?;
        found.extend(keys);
        if next == 0 {
            return Ok(found);
        }
        cursor = next;
    }
}

/// Continues a scan of one cluster node from `cursor`.
async fn scan_routed(
    conn: &mut ClusterConnection,
    node: &SingleNodeRoutingInfo,
    mut cursor: u64,
    pattern: &str,
) -> RedisResult<Vec<String>> {
    let mut found = Vec::new();
    while cursor != 0 {
        let reply = conn
            .route_command(scan_cmd(cursor, pattern), RoutingInfo::SingleNode(node.clone()))
            .await?;
        let (next, keys): (u64, Vec<String>) = redis::from_redis_value(reply)?;
        found.extend(keys);
        cursor = next;
    }
    Ok(found)
}

/// Attempts of a command on a cluster while its slot moves.
const REDIRECT_ATTEMPTS: u32 = 8;
const REDIRECT_PAUSE: Duration = Duration::from_millis(5);

/// An error of a cluster whose slots are moving, that the next attempt does not meet.
fn redirected(e: &RedisError) -> bool {
    e.is_cluster_error() || e.kind() == ErrorKind::ClusterConnectionNotFound
}

impl ConnectionLike for RedisConn {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        match &mut self.inner {
            Inner::Single(c) => c.req_packed_command(cmd),
            Inner::Cluster(l) => {
                let l = Arc::clone(l);
                Box::pin(async move {
                    let mut attempt = 1;
                    loop {
                        match l.get().await?.req_packed_command(cmd).await {
                            Err(e) if redirected(&e) && attempt < REDIRECT_ATTEMPTS => {
                                tokio::time::sleep(REDIRECT_PAUSE * attempt).await;
                                attempt += 1;
                            }
                            other => return other,
                        }
                    }
                })
            }
        }
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        cmd: &'a Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        match &mut self.inner {
            Inner::Single(c) => c.req_packed_commands(cmd, offset, count),
            Inner::Cluster(l) => {
                let l = Arc::clone(l);
                Box::pin(async move {
                    let mut attempt = 1;
                    loop {
                        match l.get().await?.req_packed_commands(cmd, offset, count).await {
                            Err(e) if redirected(&e) && attempt < REDIRECT_ATTEMPTS => {
                                tokio::time::sleep(REDIRECT_PAUSE * attempt).await;
                                attempt += 1;
                            }
                            other => return other,
                        }
                    }
                })
            }
        }
    }

    fn get_db(&self) -> i64 {
        match &self.inner {
            Inner::Single(c) => c.get_db(),
            Inner::Cluster(_) => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cluster_prefix_is_one_hash_tag() {
        assert_eq!(Topology::Cluster.slot_prefix("antirion:"), "{antirion:}");
        assert_eq!(Topology::Standalone.slot_prefix("antirion:"), "antirion:");
        assert_eq!(Topology::parse("cluster"), Topology::Cluster);
        assert_eq!(Topology::parse("sentinel"), Topology::Standalone);
    }

    #[tokio::test]
    async fn connections_are_made_on_first_use() {
        let single = RedisConn::open("redis://127.0.0.1:1/", Topology::Standalone, 0).unwrap();
        let cluster = RedisConn::open("redis://127.0.0.1:1/", Topology::Cluster, 0).unwrap();
        assert_eq!(cluster.topology(), Topology::Cluster);
        for c in [single, cluster] {
            let mut c = c.clone();
            assert!(redis::cmd("PING").query_async::<Value>(&mut c).await.is_err());
        }
    }
}

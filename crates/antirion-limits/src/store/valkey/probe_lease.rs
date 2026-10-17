//! Half-open probe leases shared by a cluster's nodes: per deployment, a sorted set of lease ids
//! scored by their expiry on the server's clock. A lease is taken only while fewer than the limit
//! are unexpired, and the set expires with its last lease.

use super::ValkeyCounters;
use crate::store::StoreError;

/// Drops expired leases, then adds one when fewer than `ARGV[1]` remain: `ARGV[2]` is its life in
/// milliseconds and `ARGV[3]` its id. Returns 1 for a lease, 0 when every one is held.
const TAKE: &str = r"
local t = redis.call('TIME')
local now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now)
if redis.call('ZCARD', KEYS[1]) >= tonumber(ARGV[1]) then return 0 end
redis.call('ZADD', KEYS[1], now + tonumber(ARGV[2]), ARGV[3])
local last = redis.call('ZRANGE', KEYS[1], -1, -1, 'WITHSCORES')
redis.call('PEXPIREAT', KEYS[1], math.floor(tonumber(last[2])) + 1000)
return 1
";

impl ValkeyCounters {
    fn lease_key(&self, deployment: &str) -> String {
        format!("{}{{h}}probe:{deployment}", self.prefix)
    }

    /// Takes one of `limit` probe leases on `deployment` for `ttl_ms`, as lease `id`; false when
    /// every lease is held.
    pub async fn probe_lease(
        &self,
        deployment: &str,
        limit: u32,
        ttl_ms: u64,
        id: u64,
    ) -> Result<bool, StoreError> {
        let mut cmd = redis::cmd("EVAL");
        cmd.arg(TAKE)
            .arg(1)
            .arg(self.lease_key(deployment))
            .arg(limit)
            .arg(ttl_ms.max(1))
            .arg(id.to_string());
        let reply = self.run(&cmd).await?;
        Ok(matches!(reply, redis::Value::Int(1)))
    }

    /// Gives probe lease `id` on `deployment` back.
    pub async fn probe_release(&self, deployment: &str, id: u64) -> Result<(), StoreError> {
        let mut cmd = redis::cmd("ZREM");
        cmd.arg(self.lease_key(deployment)).arg(id.to_string());
        self.run(&cmd).await.map(|_| ())
    }
}

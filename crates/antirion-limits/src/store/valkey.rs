//! The shared counter store on Valkey (or Redis): one function call per admission, attempt and
//! settlement, each atomic on the server, with the server's clock as the one clock. On a Redis
//! Cluster every key is under one hash tag (the prefix), so each call's keys share a slot and stay
//! atomic; the cluster client follows the slot when it moves.

use antirion_redis::{RedisConn, Topology};
use redis::{Cmd, Value};

use super::{
    AdmitDecision, AdmitRequest, AttemptRequest, BudgetChange, CounterStore, CounterView, ExpiredReservation,
    NegativeSpend, RefusalKind, ReservationId, ResvState, SettleOutcome, SettleRequest, StoreError,
    StoreRefusal,
};
use crate::counter::{CounterId, Hold, HoldMode, Rule};
use crate::math::{Gcra, Window};
use crate::period::PeriodBounds;
use antirion_core::UtcMicros;

/// The function library, loaded with `FUNCTION LOAD REPLACE`.
pub const LIBRARY: &str = include_str!("antirion.lua");

/// Counters on a Valkey or Redis server, or a Redis Cluster.
#[derive(Clone)]
pub struct ValkeyCounters {
    conn: RedisConn,
    prefix: String,
    keep_ms: u64,
}

impl std::fmt::Debug for ValkeyCounters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValkeyCounters")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

fn store_err(e: &redis::RedisError) -> StoreError {
    if e.is_io_error() || e.is_timeout() || e.is_connection_dropped() || e.is_unrecoverable_error() {
        StoreError::Unavailable(e.to_string())
    } else {
        StoreError::Protocol(e.to_string())
    }
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::BulkString(b) => Some(String::from_utf8_lossy(b).into_owned()),
        Value::SimpleString(s) => Some(s.clone()),
        Value::Int(i) => Some(i.to_string()),
        Value::Okay => Some("OK".into()),
        _ => None,
    }
}

fn items(v: &Value) -> &[Value] {
    match v {
        Value::Array(a) | Value::Set(a) => a,
        _ => &[],
    }
}

/// `s` with the glob metacharacters of `SCAN MATCH` escaped (a cluster prefix has braces).
fn glob_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn proto(what: &str) -> StoreError {
    StoreError::Protocol(format!("unexpected reply: {what}"))
}

fn num(v: &Value) -> Result<u128, StoreError> {
    text(v)
        .and_then(|s| s.parse::<u128>().ok())
        .ok_or_else(|| proto("not an unsigned integer"))
}

fn head(v: &Value) -> Result<(String, &[Value]), StoreError> {
    let all = items(v);
    let (first, rest) = all.split_first().ok_or_else(|| proto("empty reply"))?;
    Ok((text(first).ok_or_else(|| proto("no status"))?, rest))
}

/// The six function arguments of a hold: algorithm, mode, three parameters, reservation.
fn hold_args(h: &Hold) -> [String; 6] {
    let mode = match h.mode {
        HoldMode::Enforce => "E",
        HoldMode::Shadow => "S",
        HoldMode::Track => "T",
        HoldMode::Headroom => "H",
    };
    let (algo, p1, p2, p3) = match &h.rule {
        Rule::Gcra(g) => (
            "G",
            g.limit.to_string(),
            g.per_us.to_string(),
            g.burst.to_string(),
        ),
        Rule::Window(w) => (
            "W",
            w.limit.to_string(),
            w.per_us.to_string(),
            w.precision.to_string(),
        ),
        Rule::Rolling(w) => (
            "R",
            w.limit.to_string(),
            w.per_us.to_string(),
            w.precision.to_string(),
        ),
        Rule::Concurrency { limit } => ("C", limit.to_string(), "0".into(), "0".into()),
        Rule::Budget { amount, period } => (
            "B",
            amount.to_string(),
            period.end.map(|e| e.0.to_string()).unwrap_or_default(),
            "0".into(),
        ),
    };
    [algo.into(), mode.into(), p1, p2, p3, h.amount.to_string()]
}

fn views(v: &Value) -> Result<Vec<CounterView>, StoreError> {
    items(v)
        .iter()
        .map(|row| match items(row) {
            [r, f, s, res] => Ok(CounterView {
                remaining: num(r)?,
                full_in_us: u64::try_from(num(f)?).unwrap_or(u64::MAX),
                spent: num(s)?,
                reserved: num(res)?,
            }),
            _ => Err(proto("view")),
        })
        .collect()
}

fn admit_decision(v: &Value) -> Result<AdmitDecision, StoreError> {
    let (status, rest) = head(v)?;
    match status.as_str() {
        "admitted" => Ok(AdmitDecision::Admitted {
            views: views(rest.first().ok_or_else(|| proto("views"))?)?,
        }),
        "replay" => {
            let st = rest.first().and_then(text).unwrap_or_default();
            Ok(AdmitDecision::Replayed(
                ResvState::parse(&st).ok_or_else(|| proto("state"))?,
            ))
        }
        "refused" => {
            let refusals = items(rest.first().ok_or_else(|| proto("refusals"))?)
                .iter()
                .map(|r| match items(r) {
                    [i, kind, wait] => {
                        let index = usize::try_from(num(i)?).unwrap_or(0).saturating_sub(1);
                        let kind = match text(kind).as_deref() {
                            Some("rate") => RefusalKind::Rate,
                            Some("exceeds") => RefusalKind::Exceeds,
                            Some("budget") => RefusalKind::Budget,
                            _ => return Err(proto("refusal kind")),
                        };
                        let wait_us = text(wait)
                            .filter(|w| !w.is_empty())
                            .and_then(|w| w.parse::<u128>().ok())
                            .map(|w| u64::try_from(w).unwrap_or(u64::MAX));
                        Ok(StoreRefusal { index, kind, wait_us })
                    }
                    _ => Err(proto("refusal")),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(AdmitDecision::Refused { refusals })
        }
        _ => Err(proto("admission status")),
    }
}

/// Rebuilds a hold from its record entry (`algo,mode,p1,p2,p3,rho,slot`) and its counter key.
fn hold_from_record(entry: &str, key: &str) -> Option<Hold> {
    let f: Vec<&str> = entry.split(',').collect();
    let [algo, mode, p1, p2, p3, rho, _slot] = f.as_slice() else {
        return None;
    };
    let mut parts = key.rsplit(':');
    let (cid, start) = if *algo == "B" {
        let start = parts.next()?.parse::<i64>().ok()?;
        (parts.next()?, start)
    } else {
        (parts.next()?, 0)
    };
    let tag = parts.next()?;
    let group = tag
        .strip_prefix("{o")
        .and_then(|t| t.strip_suffix('}'))
        .and_then(|h| u128::from_str_radix(h, 16).ok())
        .unwrap_or(0);
    let mut id = [0u8; 16];
    for (i, slot) in id.iter_mut().enumerate() {
        *slot = u8::from_str_radix(cid.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    let u = |s: &str| s.parse::<u64>().ok();
    let rule = match *algo {
        "G" => Rule::Gcra(Gcra {
            limit: u(p1)?,
            per_us: u(p2)?,
            burst: u(p3)?,
        }),
        "W" => Rule::Window(Window {
            limit: u(p1)?,
            per_us: u(p2)?,
            precision: p3.parse().ok()?,
        }),
        "R" => Rule::Rolling(Window {
            limit: u(p1)?,
            per_us: u(p2)?,
            precision: p3.parse().ok()?,
        }),
        "C" => Rule::Concurrency { limit: u(p1)? },
        "B" => Rule::Budget {
            amount: p1.parse().ok()?,
            period: PeriodBounds {
                start: UtcMicros(start),
                end: p2.parse::<i64>().ok().map(UtcMicros),
            },
        },
        _ => return None,
    };
    let mode = match *mode {
        "S" => HoldMode::Shadow,
        "T" => HoldMode::Track,
        "H" => HoldMode::Headroom,
        _ => HoldMode::Enforce,
    };
    Some(Hold {
        counter: CounterId(id),
        group,
        rule,
        amount: rho.parse().ok()?,
        mode,
    })
}

impl ValkeyCounters {
    /// A store on `url` (`redis://host:port/db`; on a cluster, one of its nodes). No connection is
    /// made until the first call, so a node starts even when the store is down; the library is
    /// loaded on first use.
    pub fn new(
        url: &str,
        prefix: &str,
        keep_us: u64,
        topology: Topology,
    ) -> Result<ValkeyCounters, StoreError> {
        let conn = RedisConn::open(url, topology, 1).map_err(|e| store_err(&e))?;
        Ok(ValkeyCounters {
            conn,
            prefix: topology.slot_prefix(prefix),
            keep_ms: keep_us.div_ceil(1000).max(1),
        })
    }

    /// The connection, for the other state a cluster shares on the same servers.
    #[must_use]
    pub fn connection(&self) -> &RedisConn {
        &self.conn
    }

    /// Loads (or replaces) the function library.
    pub async fn load_library(&self) -> Result<(), StoreError> {
        let mut conn = self.conn.clone();
        redis::cmd("FUNCTION")
            .arg("LOAD")
            .arg("REPLACE")
            .arg(LIBRARY)
            .query_async::<Value>(&mut conn)
            .await
            .map(|_| ())
            .map_err(|e| store_err(&e))
    }

    async fn run(&self, cmd: &Cmd) -> Result<Value, StoreError> {
        let mut conn = self.conn.clone();
        cmd.query_async::<Value>(&mut conn)
            .await
            .map_err(|e| store_err(&e))
    }

    async fn fcall(&self, function: &str, keys: &[String], args: &[String]) -> Result<Value, StoreError> {
        let mut cmd = redis::cmd("FCALL");
        cmd.arg(function).arg(keys.len()).arg(keys).arg(args);
        match self.run(&cmd).await {
            Err(StoreError::Protocol(m)) if m.contains("Function not found") => {
                self.load_library().await?;
                self.run(&cmd).await
            }
            other => other,
        }
    }

    fn tag(group: u128) -> String {
        if group == 0 {
            "{g}".into()
        } else {
            format!("{{o{group:032x}}}")
        }
    }

    fn counter_key(&self, h: &Hold) -> String {
        let tag = Self::tag(h.group);
        match &h.rule {
            Rule::Budget { period, .. } => {
                format!("{}b:{tag}:{}:{}", self.prefix, h.counter.hex(), period.start.0)
            }
            _ => format!("{}c:{tag}:{}", self.prefix, h.counter.hex()),
        }
    }

    fn resv_key(&self, group: u128, uid: ReservationId) -> String {
        format!("{}r:{}:{}", self.prefix, Self::tag(group), uid.hex())
    }

    fn node_set(&self, node: &str) -> String {
        format!("{}nr:{node}", self.prefix)
    }

    fn holds_args(holds: &[Hold], into: &mut Vec<String>) {
        into.push(holds.len().to_string());
        for h in holds {
            into.extend(hold_args(h));
        }
    }

    async fn expire_one(&self, rkey: &str, nset: &str) -> Result<Option<ExpiredReservation>, StoreError> {
        let mut get = redis::cmd("HMGET");
        get.arg(rkey).arg("k");
        let reply = self.run(&get).await?;
        let key_list = items(&reply).first().and_then(text).unwrap_or_default();
        let counter_keys: Vec<String> = key_list
            .split(';')
            .filter(|k| !k.is_empty())
            .map(str::to_owned)
            .collect();
        let mut keys = vec![rkey.to_owned(), nset.to_owned()];
        keys.extend(counter_keys.iter().cloned());
        let v = self
            .fcall("antirion_expire", &keys, &[self.keep_ms.to_string()])
            .await?;
        let (status, rest) = head(&v)?;
        if status != "expired" {
            return Ok(None);
        }
        let node = rest.first().and_then(text).unwrap_or_default();
        let entries = rest.get(1).and_then(text).unwrap_or_default();
        let holds = entries
            .split(';')
            .filter(|e| !e.is_empty())
            .zip(&counter_keys)
            .map(|(e, k)| hold_from_record(e, k).ok_or_else(|| proto("hold record")))
            .collect::<Result<Vec<_>, _>>()?;
        let uid = rkey
            .rsplit(':')
            .next()
            .and_then(|h| u128::from_str_radix(h, 16).ok())
            .map(ReservationId)
            .ok_or_else(|| proto("reservation key"))?;
        Ok(Some(ExpiredReservation { uid, node, holds }))
    }

    /// Records `node` as a live member for `ttl_ms` and returns how many members are live.
    pub async fn announce(&self, node: &str, ttl_ms: u64) -> Result<u32, StoreError> {
        let members_key = format!("{}members", self.prefix);
        let member = |n: &str| format!("{}m:{n}", self.prefix);
        let mut pipe = redis::pipe();
        pipe.cmd("SET")
            .arg(member(node))
            .arg("1")
            .arg("PX")
            .arg(ttl_ms.max(1))
            .ignore()
            .cmd("SADD")
            .arg(&members_key)
            .arg(node)
            .ignore()
            .cmd("SMEMBERS")
            .arg(&members_key);
        let mut conn = self.conn.clone();
        let (nodes,): (Vec<String>,) = pipe.query_async(&mut conn).await.map_err(|e| store_err(&e))?;
        let mut alive = 0u32;
        for n in &nodes {
            let mut exists = redis::cmd("EXISTS");
            exists.arg(member(n));
            if num(&self.run(&exists).await?)? > 0 {
                alive += 1;
            } else {
                let mut forget = redis::cmd("SREM");
                forget.arg(&members_key).arg(n);
                self.run(&forget).await?;
            }
        }
        Ok(alive.max(1))
    }

    /// Records revoked MCP sessions (hex digest, expiry in Unix seconds) until they expire.
    pub async fn share_revocations(&self, items: &[(String, i64)]) -> Result<(), StoreError> {
        if items.is_empty() {
            return Ok(());
        }
        let mut pipe = redis::pipe();
        for (digest, exp) in items {
            pipe.cmd("SET")
                .arg(format!("{}mrev:{digest}", self.prefix))
                .arg(exp.to_string())
                .arg("EXAT")
                .arg((*exp).max(1))
                .arg("NX")
                .ignore();
        }
        let mut conn = self.conn.clone();
        pipe.query_async::<()>(&mut conn).await.map_err(|e| store_err(&e))
    }

    /// Every revoked MCP session the store holds (hex digest, expiry in Unix seconds).
    pub async fn revocations(&self) -> Result<Vec<(String, i64)>, StoreError> {
        let pattern = format!("{}mrev:*", glob_escape(&self.prefix));
        let keys = self.conn.scan(&pattern).await.map_err(|e| store_err(&e))?;
        let mut out = Vec::with_capacity(keys.len());
        for chunk in keys.chunks(500) {
            let mut get = redis::cmd("MGET");
            get.arg(chunk);
            let values = self.run(&get).await?;
            for (k, v) in chunk.iter().zip(items(&values)) {
                if let (Some(digest), Some(exp)) =
                    (k.rsplit(':').next(), text(v).and_then(|t| t.parse::<i64>().ok()))
                {
                    out.push((digest.to_owned(), exp));
                }
            }
        }
        Ok(out)
    }

    /// The expiry (Unix seconds) of one revoked MCP session, by hex digest, if it was revoked.
    pub async fn revocation(&self, digest: &str) -> Result<Option<i64>, StoreError> {
        let mut get = redis::cmd("GET");
        get.arg(format!("{}mrev:{digest}", self.prefix));
        let v = self.run(&get).await?;
        Ok(text(&v).and_then(|t| t.parse().ok()))
    }

    /// A session-affinity binding by its digest, as another node wrote it.
    pub async fn binding(&self, digest: &str) -> Result<Option<String>, StoreError> {
        let mut get = redis::cmd("GET");
        get.arg(format!("{}aff:{digest}", self.prefix));
        Ok(text(&self.run(&get).await?))
    }

    /// Writes a session-affinity binding, kept for `ttl_ms`.
    pub async fn bind(&self, digest: &str, value: &str, ttl_ms: u64) -> Result<(), StoreError> {
        let mut set = redis::cmd("SET");
        set.arg(format!("{}aff:{digest}", self.prefix))
            .arg(value)
            .arg("PX")
            .arg(ttl_ms.max(1));
        self.run(&set).await.map(|_| ())
    }

    /// Deletes the session-affinity bindings whose value `matches`; returns how many it deleted.
    pub async fn unbind_where(&self, matches: impl Fn(&str) -> bool) -> Result<usize, StoreError> {
        let pattern = format!("{}aff:*", glob_escape(&self.prefix));
        let keys = self.conn.scan(&pattern).await.map_err(|e| store_err(&e))?;
        let mut doomed = Vec::new();
        for chunk in keys.chunks(500) {
            let mut get = redis::cmd("MGET");
            get.arg(chunk);
            let values = self.run(&get).await?;
            for (k, v) in chunk.iter().zip(items(&values)) {
                if text(v).is_some_and(|t| matches(&t)) {
                    doomed.push(k.clone());
                }
            }
        }
        let mut deleted = 0;
        for chunk in doomed.chunks(500) {
            let mut del = redis::cmd("DEL");
            del.arg(chunk);
            deleted += usize::try_from(num(&self.run(&del).await?)?).unwrap_or(usize::MAX);
        }
        Ok(deleted)
    }

    /// Evaluates the library's arithmetic with an explicit clock (differential tests).
    pub async fn eval(&self, args: &[String]) -> Result<Vec<String>, StoreError> {
        let v = self.fcall("antirion_eval", &[], args).await?;
        Ok(items(&v).iter().filter_map(text).collect())
    }
}

impl CounterStore for ValkeyCounters {
    async fn admit(&self, req: &AdmitRequest<'_>) -> Result<AdmitDecision, StoreError> {
        let mut keys = vec![
            self.resv_key(req.group, req.uid),
            self.node_set(req.node),
            format!("{}nodes", self.prefix),
        ];
        keys.extend(req.holds.iter().map(|h| self.counter_key(h)));
        let mut args = vec![req.node.to_owned()];
        Self::holds_args(req.holds, &mut args);
        admit_decision(&self.fcall("antirion_admit", &keys, &args).await?)
    }

    async fn admit_attempt(&self, req: &AttemptRequest<'_>) -> Result<AdmitDecision, StoreError> {
        let mut keys = vec![self.resv_key(req.group, req.uid)];
        keys.extend(req.holds.iter().map(|h| self.counter_key(h)));
        let mut args = vec![req.attempt.to_string()];
        Self::holds_args(req.holds, &mut args);
        admit_decision(&self.fcall("antirion_admit_attempt", &keys, &args).await?)
    }

    async fn reconcile(&self, req: &SettleRequest<'_>) -> Result<SettleOutcome, StoreError> {
        let mut keys = vec![self.resv_key(req.group, req.uid), self.node_set(req.node)];
        keys.extend(req.holds.iter().map(|h| self.counter_key(h)));
        let mut args = vec![
            req.keep_us.div_ceil(1000).max(1).to_string(),
            req.actual.len().to_string(),
        ];
        args.extend(req.actual.iter().map(u128::to_string));
        let v = self.fcall("antirion_reconcile", &keys, &args).await?;
        let (status, rest) = head(&v)?;
        match status.as_str() {
            "settled" => {
                let budgets = items(rest.first().ok_or_else(|| proto("changes"))?)
                    .iter()
                    .map(|c| match items(c) {
                        [i, b, a] => Ok(BudgetChange {
                            index: usize::try_from(num(i)?).unwrap_or(0).saturating_sub(1),
                            before: num(b)?,
                            after: num(a)?,
                        }),
                        _ => Err(proto("budget change")),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(SettleOutcome::Settled { budgets })
            }
            "noop" => Ok(SettleOutcome::Noop(
                rest.first().and_then(text).as_deref().and_then(ResvState::parse),
            )),
            _ => Err(proto("settlement status")),
        }
    }

    async fn heartbeat(&self, node: &str, ttl_us: u64) -> Result<(), StoreError> {
        let mut pipe = redis::pipe();
        pipe.cmd("SET")
            .arg(format!("{}n:{node}", self.prefix))
            .arg("1")
            .arg("PX")
            .arg(ttl_us.div_ceil(1000).max(1))
            .ignore()
            .cmd("SADD")
            .arg(format!("{}nodes", self.prefix))
            .arg(node)
            .ignore();
        let mut conn = self.conn.clone();
        pipe.query_async::<()>(&mut conn).await.map_err(|e| store_err(&e))
    }

    async fn reap(&self, max: usize) -> Result<Vec<ExpiredReservation>, StoreError> {
        let nodes_key = format!("{}nodes", self.prefix);
        let mut members = redis::cmd("SMEMBERS");
        members.arg(&nodes_key);
        let nodes: Vec<String> = items(&self.run(&members).await?)
            .iter()
            .filter_map(text)
            .collect();
        let mut out = Vec::new();
        for node in nodes {
            let mut alive = redis::cmd("EXISTS");
            alive.arg(format!("{}n:{node}", self.prefix));
            if num(&self.run(&alive).await?)? > 0 {
                continue;
            }
            let nset = self.node_set(&node);
            let mut held = redis::cmd("SMEMBERS");
            held.arg(&nset);
            let rkeys: Vec<String> = items(&self.run(&held).await?).iter().filter_map(text).collect();
            if rkeys.is_empty() {
                let mut forget = redis::cmd("SREM");
                forget.arg(&nodes_key).arg(&node);
                self.run(&forget).await?;
                continue;
            }
            for rkey in rkeys {
                if out.len() >= max {
                    return Ok(out);
                }
                if let Some(e) = self.expire_one(&rkey, &nset).await? {
                    out.push(e);
                }
            }
        }
        Ok(out)
    }

    async fn read(&self, holds: &[Hold]) -> Result<Vec<CounterView>, StoreError> {
        let keys: Vec<String> = holds.iter().map(|h| self.counter_key(h)).collect();
        let mut args = Vec::new();
        Self::holds_args(holds, &mut args);
        views(&self.fcall("antirion_read", &keys, &args).await?)
    }

    async fn seed_budget(&self, hold: &Hold, spent: u128) -> Result<(), StoreError> {
        let end = hold_args(hold)[3].clone();
        self.fcall(
            "antirion_seed",
            &[self.counter_key(hold)],
            &[spent.to_string(), end],
        )
        .await?;
        Ok(())
    }

    async fn seed_window(&self, hold: &Hold, slots: &[(i64, u128)]) -> Result<(), StoreError> {
        let [_, _, _, per, precision, _] = hold_args(hold);
        let mut args = vec![per, precision];
        for (k, v) in slots {
            args.push(k.to_string());
            args.push(v.to_string());
        }
        self.fcall("antirion_seed_window", &[self.counter_key(hold)], &args)
            .await?;
        Ok(())
    }

    async fn adjust_budget(
        &self,
        hold: &Hold,
        delta: i128,
    ) -> Result<Result<u128, NegativeSpend>, StoreError> {
        let [algo, _, _, p2, p3, _] = hold_args(hold);
        let (function, args) = if algo == "R" {
            ("antirion_adjust_window", vec![delta.to_string(), p2, p3])
        } else {
            ("antirion_adjust", vec![delta.to_string(), p2])
        };
        let v = self.fcall(function, &[self.counter_key(hold)], &args).await?;
        let (status, rest) = head(&v)?;
        match status.as_str() {
            "ok" => Ok(Ok(num(rest.first().ok_or_else(|| proto("spend"))?)?)),
            "negative" => Ok(Err(NegativeSpend)),
            _ => Err(proto("adjustment status")),
        }
    }

    async fn ping(&self) -> Result<(), StoreError> {
        self.run(&redis::cmd("PING")).await.map(|_| ())
    }
}

mod health;
mod inflight;
mod probe_lease;

#[cfg(test)]
mod tests;

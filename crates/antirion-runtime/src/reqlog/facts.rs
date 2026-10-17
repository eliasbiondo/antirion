//! What analytics count of one record: its dimension values and measures, read from the record
//! itself and from the facts kept beside it (the key's owner, the served leaf, the environment).

use antirion_limits::money::Nusd;
use antirion_store::logs::{LogRow, MEASURES, ROLLUP_DIMS, set};
use serde_json::Value;

/// Facts the record does not carry, kept beside it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extras {
    /// The key's owner.
    pub user: Option<String>,
    /// The served leaf's path in the route tree.
    pub route_node: Option<String>,
}

/// How many measures a rollup holds.
pub(crate) const N: usize = MEASURES.len();

/// Indexes into [`MEASURES`].
pub(crate) mod m {
    pub(crate) const REQUESTS: usize = 0;
    pub(crate) const ERRORS: usize = 1;
    pub(crate) const CANCELLED: usize = 2;
    pub(crate) const FALLBACKS: usize = 3;
    pub(crate) const RETRIES: usize = 4;
    pub(crate) const CACHE_HITS: usize = 5;
    pub(crate) const BLOCKS: usize = 6;
    pub(crate) const REDACTIONS: usize = 7;
    pub(crate) const FLAGS: usize = 8;
    pub(crate) const INPUT: usize = 9;
    pub(crate) const CACHE_READ: usize = 10;
    pub(crate) const CACHE_WRITE: usize = 11;
    pub(crate) const OUTPUT: usize = 12;
    pub(crate) const REASONING: usize = 13;
    pub(crate) const COST: usize = 14;
    pub(crate) const SAVED: usize = 15;
}

/// Indexes into [`ROLLUP_DIMS`].
pub(crate) mod d {
    pub(crate) const KEY: usize = 3;
    pub(crate) const STATUS_CODE: usize = 11;
}

/// One record as analytics see it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    pub started_us: i64,
    /// Values of [`ROLLUP_DIMS`], in order.
    pub dims: Vec<Option<String>>,
    /// Values of [`MEASURES`], in order.
    pub counts: [i64; N],
    pub latency_ms: f64,
    pub ttft_ms: Option<f64>,
    /// Output tokens per second, for streamed requests.
    pub otps: Option<f64>,
    pub end_user: Option<String>,
    pub tags: Vec<String>,
    pub guardrails: Vec<String>,
    pub categories: Vec<String>,
    pub verdicts: Vec<String>,
    pub severities: Vec<String>,
    pub client_ip: Option<String>,
    pub client_country: Option<String>,
    /// The record's `cache.prefix`.
    pub cache_prefix: Option<String>,
    pub flagged: bool,
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_owned)
}

fn u(v: &Value, pointer: &str) -> i64 {
    v.pointer(pointer)
        .and_then(Value::as_u64)
        .and_then(|n| i64::try_from(n).ok())
        .unwrap_or(0)
}

/// The value of the first tag `<key>:<value>`.
#[must_use]
pub fn environment_of(tags: &[String], key: &str) -> Option<String> {
    tags.iter().find_map(|t| {
        t.split_once(':')
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v.to_owned())
    })
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn nusd(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_str)
        .and_then(|s| Nusd::parse_usd(s).ok())
        .and_then(|n| i64::try_from(n.0).ok())
}

const NON_PASS: [&str; 5] = ["block", "redact", "flag", "divert", "error_open"];

impl Facts {
    /// The facts of `record`; `environment` is the environment tag's value.
    #[must_use]
    pub fn of(record: &Value, extras: &Extras, environment: Option<String>) -> Facts {
        let status = record.get("status_code").and_then(Value::as_u64).unwrap_or(0);
        let attempts = record.get("attempts").and_then(Value::as_array);
        let reasons = || {
            attempts
                .into_iter()
                .flatten()
                .filter_map(|a| a.get("reason").and_then(Value::as_str))
        };
        let served = attempts
            .and_then(|a| a.last())
            .and_then(|a| a.get("outcome"))
            .and_then(Value::as_str)
            == Some("success");
        let guardrails: Vec<&Value> = record
            .get("guardrails")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        let field = |g: &Value, k: &str| g.get(k).and_then(Value::as_str).map(str::to_owned);
        let verdicts: Vec<String> = guardrails.iter().filter_map(|g| field(g, "verdict")).collect();
        let has = |v: &str| i64::from(verdicts.iter().any(|x| x == v));
        let cache = record
            .pointer("/cache/status")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let stream = record.get("stream").is_some_and(|v| !v.is_null());
        let latency_ms = record
            .pointer("/timing/total_ms")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let ttft_ms = record.pointer("/timing/ttft_ms").and_then(Value::as_f64);
        let output = u(record, "/usage/output");
        // Output speed is read from answered streams that took long enough to time: a stream cut
        // short, or one whose tokens all came at once, says nothing about the model's speed.
        let otps = (stream && status < 400 && record.get("code").and_then(Value::as_str).is_none())
            .then(|| {
                let generating = latency_ms - ttft_ms.unwrap_or(0.0);
                (generating >= MIN_GENERATING_MS && output > 1).then(|| output as f64 / (generating / 1000.0))
            })
            .flatten();
        let mut counts = [0i64; N];
        counts[m::REQUESTS] = 1;
        counts[m::ERRORS] = i64::from(status >= 400);
        counts[m::CANCELLED] = i64::from(record.get("client_disconnected") == Some(&Value::Bool(true)));
        counts[m::FALLBACKS] = i64::from(served && reasons().any(|r| r == "fallback"));
        counts[m::RETRIES] = i64::try_from(reasons().filter(|r| *r == "retry").count()).unwrap_or(0);
        counts[m::CACHE_HITS] = i64::from(matches!(cache.as_deref(), Some("hit" | "semantic_hit")));
        counts[m::BLOCKS] = has("block");
        counts[m::REDACTIONS] = has("redact");
        counts[m::FLAGS] = has("flag");
        counts[m::INPUT] = u(record, "/usage/input");
        counts[m::CACHE_READ] = u(record, "/usage/input_cache_read");
        counts[m::CACHE_WRITE] = u(record, "/usage/input_cache_write");
        counts[m::OUTPUT] = output;
        counts[m::REASONING] = u(record, "/usage/output_reasoning");
        counts[m::COST] = nusd(record.pointer("/cost/total")).unwrap_or(0);
        counts[m::SAVED] = nusd(record.pointer("/cost/saved")).unwrap_or(0);
        let dims = ROLLUP_DIMS
            .iter()
            .map(|d| match *d {
                "key_name" => s(record, "key"),
                "user_name" => extras.user.clone(),
                "route_node" => extras.route_node.clone(),
                "status_code" => Some(status.to_string()),
                "cache_status" => cache.clone(),
                "environment" => environment.clone(),
                "stream" => Some(stream.to_string()),
                "cache_prefix" => record
                    .pointer("/cache/prefix")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                other => s(record, other),
            })
            .collect();
        Facts {
            started_us: record
                .get("started_at")
                .and_then(Value::as_str)
                .and_then(|t| antirion_core::UtcMicros::parse_rfc3339(t).ok())
                .map_or(0, |t| t.0),
            dims,
            counts,
            latency_ms,
            ttft_ms,
            otps,
            end_user: s(record, "end_user"),
            tags: strings(record.get("tags")),
            guardrails: guardrails.iter().filter_map(|g| field(g, "name")).collect(),
            categories: guardrails.iter().filter_map(|g| field(g, "category")).collect(),
            severities: guardrails.iter().filter_map(|g| field(g, "severity")).collect(),
            flagged: verdicts.iter().any(|v| NON_PASS.contains(&v.as_str())),
            verdicts,
            client_ip: s(record, "client_ip"),
            client_country: record
                .pointer("/client_geo/country")
                .and_then(Value::as_str)
                .map(str::to_owned),
            cache_prefix: record
                .pointer("/cache/prefix")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
    }

    fn dim(&self, name: &str) -> Option<String> {
        ROLLUP_DIMS
            .iter()
            .position(|d| *d == name)
            .and_then(|i| self.dims.get(i).cloned().flatten())
    }

    /// The store row of `record`: its indexed columns, its JSON without content, and its content.
    #[must_use]
    pub fn row(&self, record: &Value, node_id: &str) -> LogRow {
        let mut bare = record.clone();
        let content = bare
            .as_object_mut()
            .and_then(|o| o.insert("content".into(), Value::Null))
            .filter(|c| !c.is_null());
        let prompt = content
            .as_ref()
            .and_then(|c| c.get("request"))
            .filter(|r| !r.is_null())
            .map(Value::to_string);
        let cost = nusd(record.pointer("/cost/total")).unwrap_or(-1);
        LogRow {
            id: s(record, "id").unwrap_or_default(),
            started_us: self.started_us,
            node_id: node_id.to_owned(),
            request_id: s(record, "request_id").unwrap_or_default(),
            organization: self.dim("organization"),
            team: self.dim("team"),
            project: self.dim("project"),
            key: self.dim("key_name"),
            user: self.dim("user_name"),
            model: self.dim("model"),
            requested_model: self.dim("requested_model"),
            provider: self.dim("provider"),
            deployment: self.dim("deployment"),
            route: self.dim("route"),
            route_node: self.dim("route_node"),
            status_code: record.get("status_code").and_then(Value::as_i64).unwrap_or(0),
            code: self.dim("code"),
            response_path: s(record, "response_path").unwrap_or_default(),
            operation: s(record, "operation").unwrap_or_default(),
            end_user: self.end_user.clone(),
            session_id: s(record, "session_id"),
            environment: self.dim("environment"),
            tags: set(self.tags.iter().map(String::as_str)),
            guardrails: set(self.guardrails.iter().map(String::as_str)),
            verdicts: set(self.verdicts.iter().map(String::as_str)),
            categories: set(self.categories.iter().map(String::as_str)),
            flagged: self.flagged,
            stream: self.dim("stream").as_deref() == Some("true"),
            cache: self.dim("cache_status"),
            latency_ms: self.latency_ms,
            ttft_ms: self.ttft_ms.unwrap_or(-1.0),
            cost_nusd: cost,
            input_tokens: self.counts[m::INPUT],
            output_tokens: self.counts[m::OUTPUT],
            client_ip: self.client_ip.clone(),
            record: bare.to_string(),
            content: content.map(|c| c.to_string()),
            prompt,
        }
    }
}

/// The shortest generation an output speed is measured over.
const MIN_GENERATING_MS: f64 = 50.0;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn record() -> Value {
        json!({
            "id": "l1", "request_id": "r1", "started_at": "2026-09-30T12:00:00.000000Z",
            "organization": "o", "team": "t", "project": "p", "key": "k",
            "requested_model": "alias", "model": "m", "route": "rt", "provider": "pv", "deployment": "dp",
            "status_code": 200, "code": null, "response_path": "upstream", "operation": "chat",
            "stream": {"end_reason": "complete"}, "client_ip": "203.0.113.7", "client_disconnected": false,
            "tags": ["env:prod", "x"], "end_user": "u1",
            "attempts": [{"reason": "primary", "outcome": "upstream_5xx"}, {"reason": "retry", "outcome": "timeout"},
                         {"reason": "fallback", "outcome": "success"}],
            "guardrails": [{"name": "pii", "verdict": "redact", "category": "pii", "severity": "high"},
                           {"name": "kw", "verdict": "pass"}],
            "cache": {"status": "miss"},
            "usage": {"input": 10, "output": 50, "input_cache_read": 2},
            "cost": {"total": "0.00012"},
            "timing": {"total_ms": 1100.0, "ttft_ms": 100.0},
            "content": {"request": {"messages": [{"role": "user", "content": "hi"}]}, "response": null},
        })
    }

    #[test]
    fn no_output_speed_from_a_stream_too_short_to_time_or_cut_short() {
        let extras = Extras::default();
        let mut instant = record();
        instant["timing"]["ttft_ms"] = instant["timing"]["total_ms"].clone();
        let f = Facts::of(&instant, &extras, None);
        assert_eq!(f.otps, None);
        let mut cut = record();
        cut["code"] = serde_json::json!("upstream_truncated");
        assert_eq!(Facts::of(&cut, &extras, None).otps, None);
    }

    #[test]
    fn measures_and_dimensions_of_a_record() {
        let extras = Extras {
            user: Some("alice".into()),
            route_node: Some("rt.tree.children[1]".into()),
        };
        let r = record();
        let f = Facts::of(&r, &extras, environment_of(&strings(r.get("tags")), "env"));
        assert_eq!(f.counts[m::FALLBACKS], 1);
        assert_eq!(f.counts[m::RETRIES], 1);
        assert_eq!(f.counts[m::REDACTIONS], 1);
        assert_eq!(f.counts[m::ERRORS], 0);
        assert_eq!(f.counts[m::COST], 120_000);
        assert_eq!(f.otps, Some(50.0));
        assert!(f.flagged);
        assert_eq!(f.dim("environment").as_deref(), Some("prod"));
        assert_eq!(f.dim("user_name").as_deref(), Some("alice"));
        assert_eq!(f.dim("stream").as_deref(), Some("true"));
        let row = f.row(&r, "n1");
        assert_eq!(row.cost_nusd, 120_000);
        assert!(row.prompt.unwrap().contains("hi"));
        assert!(
            !row.record.contains("\"hi\""),
            "content is kept apart from the record"
        );
        assert_eq!(row.route_node.as_deref(), Some("rt.tree.children[1]"));
        assert_eq!(row.tags, "\u{1f}env:prod\u{1f}x\u{1f}");
    }
}

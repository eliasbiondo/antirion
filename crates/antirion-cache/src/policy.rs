//! The effective `policies.cache` of one request, read from the member merged over every level.

use std::time::Duration;

use antirion_config::scalar::{parse_duration, parse_size};
use serde_json::Value;

use crate::semantic::Lexicon;

/// Every endpoint the exact cache serves in this build, by canonical operation name.
pub const CACHEABLE: &[&str] = &[
    "openai.chat_completions",
    "openai.completions",
    "openai.responses",
    "anthropic.messages",
    "openai.embeddings",
    "rerank",
];

/// Who shares entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    Organization,
    Team,
    Project,
    Key,
    EndUser,
    Shared(String),
}

/// The breakpoints a request may get.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Tools,
    System,
    Messages,
}

/// The `prompt_caching` block in force.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptCaching {
    pub auto_inject: bool,
    pub targets: Vec<Target>,
    pub min_prefix_tokens: u64,
    pub message_offset: u64,
    /// `ttl: "1h"`.
    pub one_hour: bool,
    /// `openai_cache_key`: OpenAI attempts carry a `prompt_cache_key` of the project and route.
    pub openai_cache_key: bool,
}

/// The `policies.cache` in effect for a request.
#[derive(Clone, Debug)]
pub struct CachePolicy {
    pub enabled: bool,
    /// `activation: opt_in`: only requests with the `use` directive are cached.
    pub opt_in: bool,
    pub scope: Scope,
    pub ttl: Duration,
    pub max_ttl: Duration,
    pub endpoints: Vec<String>,
    pub max_entry_size: u64,
    pub store_after_fallback: bool,
    pub include_headers: Vec<String>,
    pub ignore_fields: Vec<String>,
    /// Directive names clients may send, in the policy's spelling (`no_cache`).
    pub overridable: Vec<String>,
    pub threshold: f64,
    pub prompt: Option<PromptCaching>,
    /// More messages than this make a request uncacheable.
    pub max_messages: Option<u64>,
    /// The semantic layer, when enabled.
    pub semantic: Option<SemanticPolicy>,
    /// `coalesce.max_wait`, when identical concurrent misses are coalesced.
    pub coalesce: Option<Duration>,
    /// `read_only`: hits are served, nothing is written.
    pub read_only: bool,
    /// `fail_mode: closed`: a failed lookup refuses the request instead of calling the upstream.
    pub fail_closed: bool,
    /// `embeddings.per_item`: each element of an embeddings `input` array is cached on its own.
    pub per_item_embeddings: bool,
}

/// The semantic layer of a policy.
#[derive(Clone, Debug)]
pub struct SemanticPolicy {
    /// The public Model that embeds the query.
    pub embedding_model: String,
    pub max_text_chars: usize,
    /// The polarity guard's lexicon, when the guard is on.
    pub polarity: Option<Lexicon>,
}

impl SemanticPolicy {
    fn from_value(v: Option<&Value>) -> Option<SemanticPolicy> {
        let v = v?;
        if v.get("enabled").and_then(Value::as_bool) != Some(true) {
            return None;
        }
        Some(SemanticPolicy {
            embedding_model: v.get("embedding_model")?.as_str()?.to_owned(),
            max_text_chars: v
                .get("max_text_chars")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(8192),
            polarity: (v.get("polarity_guard").and_then(Value::as_bool) != Some(false))
                .then(|| Lexicon::from_value(v.get("polarity_lexicon"))),
        })
    }
}

const OVERRIDABLE: &[&str] = &[
    "no_cache",
    "no_store",
    "bypass",
    "max_age",
    "ttl",
    "namespace",
    "threshold",
    "use",
];

fn strings(v: Option<&Value>) -> Option<Vec<String>> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
}

impl CachePolicy {
    /// The policy of the merged `policies.cache` member (an empty object when no level sets it).
    #[must_use]
    pub fn from_value(v: &Value) -> CachePolicy {
        let duration = |name: &str, default: u64| {
            v.get(name)
                .and_then(Value::as_str)
                .and_then(parse_duration)
                .unwrap_or(Duration::from_secs(default))
        };
        let scope = match v.get("scope").and_then(Value::as_str) {
            Some("organization") => Scope::Organization,
            Some("team") => Scope::Team,
            Some("key") => Scope::Key,
            Some("end_user") => Scope::EndUser,
            Some("shared") => Scope::Shared(
                v.get("shared_namespace")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            _ => Scope::Project,
        };
        CachePolicy {
            enabled: CachePolicy::enabled_in(v),
            opt_in: v.get("activation").and_then(Value::as_str) == Some("opt_in"),
            scope,
            ttl: duration("ttl", 3600),
            max_ttl: duration("max_ttl", 7 * 86_400),
            endpoints: strings(v.get("endpoints"))
                .unwrap_or_else(|| CACHEABLE.iter().map(|s| (*s).to_owned()).collect()),
            max_entry_size: v
                .get("max_entry_size")
                .and_then(Value::as_str)
                .and_then(parse_size)
                .unwrap_or(1 << 20),
            store_after_fallback: v.get("store_after_fallback").and_then(Value::as_bool) == Some(true),
            include_headers: strings(v.pointer("/key/include_headers")).unwrap_or_else(|| {
                ["anthropic-version", "anthropic-beta", "openai-beta"]
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect()
            }),
            ignore_fields: strings(v.pointer("/key/ignore_fields")).unwrap_or_default(),
            overridable: strings(v.get("request_overridable"))
                .unwrap_or_else(|| OVERRIDABLE.iter().map(|s| (*s).to_owned()).collect()),
            threshold: v
                .pointer("/semantic/threshold")
                .and_then(Value::as_f64)
                .unwrap_or(0.95),
            prompt: CachePolicy::prompt_in(v),
            max_messages: v.get("max_messages").and_then(Value::as_u64),
            semantic: SemanticPolicy::from_value(v.get("semantic")),
            coalesce: (v.pointer("/coalesce/enabled").and_then(Value::as_bool) == Some(true)).then(|| {
                v.pointer("/coalesce/max_wait")
                    .and_then(Value::as_str)
                    .and_then(parse_duration)
                    .unwrap_or(Duration::from_secs(30))
            }),
            read_only: v.get("read_only").and_then(Value::as_bool) == Some(true),
            fail_closed: v.get("fail_mode").and_then(Value::as_str) == Some("closed"),
            per_item_embeddings: v.pointer("/embeddings/per_item").and_then(Value::as_bool) == Some(true),
        }
    }

    /// Whether the merged member `v` turns the cache on: [`CachePolicy::enabled`] without the
    /// rest of the policy.
    #[must_use]
    pub fn enabled_in(v: &Value) -> bool {
        v.get("enabled").and_then(Value::as_bool) == Some(true)
    }

    /// The `prompt_caching` block of the merged member `v`: [`CachePolicy::prompt`] without the
    /// rest of the policy.
    #[must_use]
    pub fn prompt_in(v: &Value) -> Option<PromptCaching> {
        v.get("prompt_caching").map(PromptCaching::from_value)
    }

    /// Whether a body has more messages than `max_messages`: Chat and Anthropic `messages`, the
    /// Responses `input` items (a string is one). Endpoints without messages never exceed it.
    #[must_use]
    pub fn too_many_messages(&self, op: &str, body: &Value) -> bool {
        let Some(max) = self.max_messages else {
            return false;
        };
        let count = match op {
            "openai.chat_completions" | "anthropic.messages" => {
                body.get("messages").and_then(Value::as_array).map(Vec::len)
            }
            "openai.responses" => match body.get("input") {
                Some(Value::Array(items)) => Some(items.len()),
                Some(Value::String(_)) => Some(1),
                _ => None,
            },
            _ => None,
        };
        count.is_some_and(|n| u64::try_from(n).unwrap_or(u64::MAX) > max)
    }

    /// Whether the policy caches requests to `endpoint`.
    #[must_use]
    pub fn covers(&self, endpoint: &str) -> bool {
        CACHEABLE.contains(&endpoint) && self.endpoints.iter().any(|e| e == endpoint)
    }
}

impl PromptCaching {
    fn from_value(v: &Value) -> PromptCaching {
        let targets = strings(v.get("targets"))
            .unwrap_or_else(|| vec!["system".into(), "tools".into(), "messages".into()])
            .iter()
            .filter_map(|t| match t.as_str() {
                "tools" => Some(Target::Tools),
                "system" => Some(Target::System),
                "messages" => Some(Target::Messages),
                _ => None,
            })
            .collect();
        PromptCaching {
            auto_inject: v.get("auto_inject").and_then(Value::as_bool) == Some(true),
            targets,
            min_prefix_tokens: v.get("min_prefix_tokens").and_then(Value::as_u64).unwrap_or(1024),
            message_offset: v.get("message_offset").and_then(Value::as_u64).unwrap_or(0),
            one_hour: v.get("ttl").and_then(Value::as_str) == Some("1h"),
            openai_cache_key: v.get("openai_cache_key").and_then(Value::as_bool) == Some(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_apply_to_an_empty_member() {
        let p = CachePolicy::from_value(&json!({}));
        assert!(!p.enabled);
        assert_eq!(p.scope, Scope::Project);
        assert_eq!(p.ttl, Duration::from_secs(3600));
        assert_eq!(p.max_entry_size, 1 << 20);
        assert!(p.covers("openai.chat_completions"));
        assert!(!p.covers("openai.files"));
        assert!(p.prompt.is_none());
        assert!(!p.too_many_messages("openai.chat_completions", &json!({"messages": [1, 2, 3, 4]})));
        assert!(p.semantic.is_none() && p.coalesce.is_none());
        assert!(!p.read_only);
        assert!(CachePolicy::from_value(&json!({"read_only": true})).read_only);
    }

    #[test]
    fn semantic_and_coalescing_are_read() {
        let p = CachePolicy::from_value(&json!({"semantic": {"enabled": true, "embedding_model": "e",
            "polarity_guard": false, "max_text_chars": 5}, "coalesce": {"enabled": true, "max_wait": "2s"}}));
        let s = p.semantic.unwrap();
        assert_eq!((s.embedding_model.as_str(), s.max_text_chars), ("e", 5));
        assert!(s.polarity.is_none());
        assert_eq!(p.coalesce, Some(Duration::from_secs(2)));
    }

    #[test]
    fn max_messages_counts_strictly_more() {
        let p = CachePolicy::from_value(&json!({"max_messages": 2}));
        let chat = |n: usize| json!({"messages": vec![json!({}); n]});
        assert!(!p.too_many_messages("openai.chat_completions", &chat(2)));
        assert!(p.too_many_messages("anthropic.messages", &chat(3)));
        assert!(!p.too_many_messages("openai.responses", &json!({"input": "hi"})));
        assert!(p.too_many_messages("openai.responses", &json!({"input": [1, 2, 3]})));
        assert!(!p.too_many_messages("openai.embeddings", &json!({"input": [1, 2, 3]})));
    }

    #[test]
    fn fields_are_read() {
        let p = CachePolicy::from_value(&json!({"enabled": true, "scope": "key", "ttl": "3s",
            "endpoints": ["anthropic.messages"], "prompt_caching": {"targets": ["tools"], "ttl": "1h"}}));
        assert!(p.enabled);
        assert_eq!(p.scope, Scope::Key);
        assert_eq!(p.ttl, Duration::from_secs(3));
        assert!(!p.covers("openai.chat_completions"));
        let pc = p.prompt.unwrap();
        assert_eq!(pc.targets, vec![Target::Tools]);
        assert!(pc.one_hour && !pc.auto_inject);
    }
}

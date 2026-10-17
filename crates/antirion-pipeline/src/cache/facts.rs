//! What an entry records about its request (`EntryMeta`), the caps it counts under, the semantic
//! group it belongs to, hits counted per entry, and the `metadata_changed` invalidation rules,
//! which a request fires before its own lookup so it never sees an answer they drop.

use std::collections::BTreeMap;
use std::sync::Arc;

use antirion_cache::meta::Cap;
use antirion_cache::rules::{Cause, Event};
use antirion_cache::{CachePolicy, EntryMeta, Mode};
use serde_json::Value;

use super::Inputs;
use crate::ctx::Ctx;

/// The caps of every level that sets `max_entries`.
fn caps(levels: &[(&str, Option<&Value>)]) -> Vec<Cap> {
    levels
        .iter()
        .filter_map(|(name, cache)| {
            let max = (*cache)?.get("max_entries")?.as_u64()?;
            Some(Cap {
                scope: (*name).to_owned(),
                max,
            })
        })
        .collect()
}

/// The narrowest level that sets `semantic.enabled`, when it turns the semantic layer on.
fn semantic_group(levels: &[(&str, Option<&Value>)]) -> Option<String> {
    let (name, enabled) = levels.iter().rev().find_map(|(name, cache)| {
        let e = (*cache)?.pointer("/semantic/enabled")?.as_bool()?;
        Some((*name, e))
    })?;
    enabled.then(|| name.to_owned())
}

/// The value of a metadata key as text.
fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// The facts an entry written for this request records.
pub(super) fn meta(cx: &Ctx, inp: &Inputs<'_>, policy: &CachePolicy, digest: &str) -> EntryMeta {
    let k = inp.key;
    let keys = &cx.snap.cache.metadata_keys;
    let metadata: BTreeMap<String, String> = inp
        .metadata
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(name, _)| keys.contains(name.as_str()))
        .filter_map(|(name, v)| Some((name.clone(), text(v)?)))
        .collect();
    let semantic = policy.semantic.is_some();
    EntryMeta {
        mode: Mode::Exact,
        digest: digest.to_owned(),
        model: inp.model.to_owned(),
        route: inp.policy_route.map(str::to_owned),
        scope: match &policy.scope {
            antirion_cache::Scope::Organization => "organization",
            antirion_cache::Scope::Team => "team",
            antirion_cache::Scope::Project => "project",
            antirion_cache::Scope::Key => "key",
            antirion_cache::Scope::EndUser => "end_user",
            antirion_cache::Scope::Shared(_) => "shared",
        }
        .to_owned(),
        organization: k.organization.clone(),
        team: k.team.clone(),
        project: k.project.clone(),
        key: k.name.clone(),
        end_user: inp.end_user.map(str::to_owned),
        tags: inp.tags.to_vec(),
        metadata,
        input_tokens: 0,
        output_tokens: 0,
        similarity_threshold: semantic.then_some(policy.threshold),
        semantic_group: if semantic {
            semantic_group(inp.levels)
        } else {
            None
        },
        origin: None,
        caps: caps(inp.levels),
    }
}

/// Counts a hit of the entry served under `key`: inline on the memory backend, on a tracked task
/// on a shared one.
pub(super) async fn count_hit(cx: &Ctx, key: &str) {
    let cache = &cx.rt.state.cache;
    let now_ms = cx.rt.now().0 / 1000;
    if cache.local() {
        if let Err(e) = cache.hit(key, now_ms).await {
            tracing::debug!(error = %e, "a cache hit could not be counted");
        }
        return;
    }
    let rt = Arc::clone(&cx.rt);
    let key = key.to_owned();
    cx.rt.tasks.spawn(async move {
        if let Err(e) = rt.state.cache.hit(&key, now_ms).await {
            tracing::debug!(error = %e, "a cache hit could not be counted");
        }
    });
}

/// Runs the `metadata_changed` rules a request's metadata fires within its cache scope: the
/// entries recorded with the previous value are dead before this request looks up.
pub(super) fn metadata_rules(cx: &Ctx, inp: &Inputs<'_>, digest: &str) {
    let rules = &cx.snap.cache.rules;
    if rules.is_empty() {
        return;
    }
    let Some(metadata) = inp.metadata.and_then(Value::as_object) else {
        return;
    };
    let now_ms = cx.rt.now().0 / 1000;
    for rule in rules.iter().filter(|r| r.enabled) {
        let Event::MetadataChanged(name) = &rule.event else {
            continue;
        };
        let Some(value) = metadata.get(name).and_then(text) else {
            continue;
        };
        let Some(old) = cx.rt.state.cache.seen.changed(&rule.name, digest, &value) else {
            continue;
        };
        let cause = Cause {
            subject: None,
            metadata: Some((name, &old, digest)),
        };
        if let Some((target, cutoff)) = rule.target(&cause, now_ms) {
            if let Some(key) = target.cutoff_key() {
                cx.rt
                    .state
                    .cache
                    .epochs
                    .raise(&key, u64::try_from(cutoff).unwrap_or(0));
            }
            let rt = Arc::clone(&cx.rt);
            let name = rule.name.clone();
            cx.rt.tasks.spawn(async move {
                rt.fire_cache_rule(&name, "metadata_changed", target, cutoff)
                    .await;
            });
        }
    }
}

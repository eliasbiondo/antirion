//! The key document of a request: tenant scope, epochs, the route and guardrail fingerprints, the
//! included headers and query, and the normalised body.

use antirion_cache::key::{KeyDoc, fingerprint, included_headers, normalise, query_pairs};
use antirion_cache::{CachePolicy, Directives, Scope};
use serde_json::{Map, Value, json};

use super::Inputs;
use crate::ctx::Ctx;

/// Query parameters the gateway owns: a credential in the query is never part of the key.
const OWNED_QUERY: &[&str] = &["key"];

/// The first 16 hex digits of the scope's digest, part of every backend key.
pub(super) fn scope_digest(scope: &[String]) -> String {
    let v = Value::Array(scope.iter().map(|s| Value::from(s.as_str())).collect());
    let mut d = fingerprint(&v);
    d.truncate(16);
    d
}

fn scope(policy: &CachePolicy, inp: &Inputs<'_>) -> Vec<String> {
    let k = inp.key;
    let project = || {
        vec![
            "project".to_owned(),
            k.organization.clone(),
            k.team.clone(),
            k.project.clone(),
        ]
    };
    match &policy.scope {
        Scope::Organization => vec!["organization".into(), k.organization.clone()],
        Scope::Team => vec!["team".into(), k.organization.clone(), k.team.clone()],
        Scope::Project => project(),
        Scope::Key => vec![
            "key".into(),
            k.organization.clone(),
            k.team.clone(),
            k.project.clone(),
            k.name.clone(),
        ],
        Scope::EndUser => match inp.end_user {
            Some(u) => vec![
                "end_user".into(),
                k.organization.clone(),
                k.team.clone(),
                k.project.clone(),
                u.to_owned(),
            ],
            None => project(),
        },
        Scope::Shared(ns) => vec!["shared".into(), ns.clone()],
    }
}

/// The route definition and every deployment it can reach, with its provider: any change to them
/// changes the key. Secret fields hold their reference text in the stored document.
fn route_fp(cx: &Ctx, inp: &Inputs<'_>) -> String {
    let doc = &cx.snap.config.doc;
    let get = |c: &str, n: &str| doc.get(c).and_then(|m| m.get(n)).cloned();
    let route = match inp.policy_route {
        Some(r) => get("routes", r),
        None => get("models", inp.model).and_then(|m| m.get("target").cloned()),
    }
    .unwrap_or_else(|| Value::from(inp.route));
    let mut names: Vec<&str> = inp.reachable.iter().map(|d| d.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    let deployments: Map<String, Value> = inp
        .reachable
        .iter()
        .map(|d| {
            let provider = get("providers", &d.provider.name).unwrap_or(Value::Null);
            let def = get("deployments", &d.name).unwrap_or_else(|| json!({"model": d.model}));
            (d.name.clone(), json!({"deployment": def, "provider": provider}))
        })
        .collect();
    fingerprint(&json!({"route": route, "deployments": deployments, "names": names}))
}

/// The input and output guardrails that apply, in order, with their definitions; the digest of
/// `[]` when none does.
fn guard_fp(attached: &[u8]) -> String {
    match serde_json::from_slice::<Value>(attached) {
        Ok(v) => fingerprint(&v),
        Err(_) if attached.is_empty() => fingerprint(&Value::Array(Vec::new())),
        Err(_) => antirion_cache::key::sha256_hex(attached),
    }
}

pub(super) fn build(
    cx: &Ctx,
    inp: &Inputs<'_>,
    policy: &CachePolicy,
    d: &Directives,
    op: &str,
    body: &Value,
) -> KeyDoc {
    let k = inp.key;
    let epochs = &cx.rt.state.cache.epochs;
    let headers: Vec<(&str, &str)> = inp
        .parts
        .headers
        .iter()
        .filter_map(|(n, v)| Some((n.as_str(), v.to_str().ok()?)))
        .collect();
    KeyDoc {
        scope: scope(policy, inp),
        epochs: [
            epochs.get("gateway", ""),
            epochs.get("organization", &k.organization),
            epochs.get("team", &k.team),
            epochs.get("project", &k.project),
            epochs.get("route", inp.policy_route.unwrap_or(inp.route)),
        ],
        namespace: d.namespace.clone().unwrap_or_default(),
        endpoint: op.to_owned(),
        route: inp.route.to_owned(),
        route_fp: route_fp(cx, inp),
        guard_fp: guard_fp(inp.guardrails),
        headers: included_headers(&policy.include_headers, &headers),
        query: query_pairs(inp.parts.uri.query(), OWNED_QUERY),
        body: normalise(op, body, &policy.ignore_fields),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_digests_are_short_hex() {
        let d = scope_digest(&["project".into(), "o".into()]);
        assert_eq!(d.len(), 16);
        assert_ne!(d, scope_digest(&["project".into(), "p".into()]));
        assert!(antirion_cache::key::sha256_hex(b"").starts_with("e3b0c442"));
    }
}

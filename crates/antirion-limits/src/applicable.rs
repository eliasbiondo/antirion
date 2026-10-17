//! The applicable set of a request: every enabled declaration along its policy chain whose filters
//! accept it, resolved to one counter per partition value.

use std::net::IpAddr;

use crate::counter::CounterId;
use crate::decl::{Algorithm, LimitUnit, MatchSpec, OnMissing, Partition, PartitionAttr};
use crate::level::Level;
use crate::policy::{DeclKind, Declared, Policies};

/// The attributes of a request that declarations match and partition on. Every value comes from
/// the authenticated context, never from a header naming what to key on.
#[derive(Clone, Copy, Debug, Default)]
pub struct RequestAttrs<'a> {
    pub organization: &'a str,
    pub team: &'a str,
    pub project: &'a str,
    pub key: &'a str,
    pub user: Option<&'a str>,
    pub end_user: Option<&'a str>,
    /// The resolved public Model.
    pub model: Option<&'a str>,
    pub route: Option<&'a str>,
    /// The endpoint family name.
    pub endpoint: &'a str,
    /// Effective tags, `name:value` or bare.
    pub tags: &'a [String],
    pub ip: Option<IpAddr>,
    pub mcp_server: Option<&'a str>,
    pub mcp_tool: Option<&'a str>,
    pub a2a_agent: Option<&'a str>,
    /// The key's priority class, for limits with priority shares.
    pub priority_class: Option<&'a str>,
}

/// Which declarations a lookup covers.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Scope<'a> {
    /// Gateway, organization, team, project, key, route and model: reserved at `admit`.
    Request,
    /// The nodes on an attempt's tree position (their route paths), and its deployment.
    Attempt {
        nodes: &'a [String],
        deployment: Option<&'a str>,
    },
}

/// A declaration that applies, with its counter and quota.
#[derive(Clone, Debug)]
pub struct Applicable<'p> {
    pub decl: &'p Declared,
    pub counter: CounterId,
    /// The quota for this partition: an override, or the declared limit or amount.
    pub quota: u128,
    /// The organization incarnation for tenant-level counters, zero otherwise.
    pub group: u128,
    /// The partition values; kept in process memory only (alert payloads), never stored.
    pub partition: Vec<(String, String)>,
    /// The counter's identity without its window: a budget's spend in the ledger is attributed to
    /// it, so a changed period is recomputed from the lines of the same budget.
    pub identity: CounterId,
}

/// A declaration with `on_missing: reject` partitions on an attribute the request lacks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the request has no {attribute}, which a limit partitions on")]
pub struct MissingAttribute {
    pub attribute: String,
}

/// `*` matches any run of characters, anywhere in the pattern.
#[must_use]
pub fn glob(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return pattern == name;
    };
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for p in middle {
        match rest.find(p) {
            Some(i) => rest = rest.get(i + p.len()..).unwrap_or(""),
            None => return false,
        }
    }
    rest.ends_with(last)
}

fn any_glob(patterns: &[String], value: Option<&str>) -> bool {
    patterns.is_empty() || value.is_some_and(|v| patterns.iter().any(|p| glob(p, v)))
}

impl RequestAttrs<'_> {
    fn matches(&self, m: &MatchSpec) -> bool {
        any_glob(&m.models, self.model)
            && (m.endpoints.is_empty() || m.endpoints.iter().any(|e| e == self.endpoint))
            && (m.tags.is_empty() || self.tags.iter().any(|t| m.tags.iter().any(|p| glob(p, t))))
            && any_glob(&m.mcp_servers, self.mcp_server)
            && any_glob(&m.mcp_tools, self.mcp_tool)
            && any_glob(&m.a2a_agents, self.a2a_agent)
    }

    fn value(&self, attr: &PartitionAttr) -> Option<String> {
        let s = |v: &str| Some(v.to_owned());
        match attr {
            PartitionAttr::Key => s(self.key),
            PartitionAttr::Project => s(self.project),
            PartitionAttr::Team => s(self.team),
            PartitionAttr::Organization => s(self.organization),
            PartitionAttr::User => self.user.map(str::to_owned),
            PartitionAttr::EndUser => self.end_user.map(str::to_owned),
            PartitionAttr::Model => self.model.map(str::to_owned),
            PartitionAttr::Endpoint => s(self.endpoint),
            PartitionAttr::Ip => self.ip.map(|ip| match ip {
                IpAddr::V4(v4) => v4.to_string(),
                IpAddr::V6(v6) => {
                    let mut seg = v6.segments();
                    for x in seg.iter_mut().skip(4) {
                        *x = 0;
                    }
                    format!("{}/64", std::net::Ipv6Addr::from(seg))
                }
            }),
            PartitionAttr::Tag(name) => self.tags.iter().find_map(|t| {
                t.strip_prefix(name.as_str())
                    .and_then(|rest| rest.strip_prefix(':'))
                    .map(str::to_owned)
            }),
            PartitionAttr::McpServer => self.mcp_server.map(str::to_owned),
            PartitionAttr::McpTool => self.mcp_tool.map(str::to_owned),
            PartitionAttr::A2aAgent => self.a2a_agent.map(str::to_owned),
        }
    }

    /// The partition values of a declaration: `Ok(None)` when the request is exempt (`skip`).
    fn partition(&self, p: &Partition) -> Result<Option<Vec<(String, String)>>, MissingAttribute> {
        let mut out = Vec::with_capacity(p.attrs.len());
        for attr in &p.attrs {
            match self.value(attr) {
                Some(v) => out.push((attr.name(), v)),
                None => match p.on_missing {
                    OnMissing::Shared => out.push((attr.name(), SHARED_VALUE.to_owned())),
                    OnMissing::Skip => return Ok(None),
                    OnMissing::Reject => {
                        return Err(MissingAttribute {
                            attribute: attr.name(),
                        });
                    }
                },
            }
        }
        Ok(Some(out))
    }
}

/// The part of a counter's identity that changes when its window does: a changed algorithm,
/// budget period or sliding-window `per` starts a fresh counter. A GCRA counter keeps its state
/// across a changed `per` and rescales it (see [`crate::math::GcraState`]).
fn window_key(kind: &DeclKind) -> String {
    match kind {
        DeclKind::Limit(l) => match (&l.unit, l.algorithm) {
            (LimitUnit::Concurrency, _) => "concurrency".into(),
            (_, Algorithm::Gcra) => "gcra".into(),
            (_, Algorithm::SlidingWindow { precision }) => format!("window:{}:{precision}", l.per_us),
        },
        DeclKind::Budget(b) => format!("budget:{}", b.period_key),
    }
}

fn owners<'a>(a: &RequestAttrs<'a>, scope: Scope<'a>) -> Vec<(Level, &'a str)> {
    match scope {
        Scope::Request => {
            let mut v = vec![
                (Level::Gateway, "-"),
                (Level::Organization, a.organization),
                (Level::Team, a.team),
                (Level::Project, a.project),
                (Level::Key, a.key),
            ];
            v.extend(a.route.map(|r| (Level::Route, r)));
            v.extend(a.model.map(|m| (Level::Model, m)));
            v
        }
        Scope::Attempt { nodes, deployment } => {
            let mut v: Vec<(Level, &str)> = nodes.iter().map(|n| (Level::Node, n.as_str())).collect();
            v.extend(deployment.map(|d| (Level::Deployment, d)));
            v
        }
    }
}

/// The applicable set of a request in a scope. Rate limits are left out when `limits.enabled` is
/// false; budgets always apply.
pub fn applicable<'p>(
    p: &'p Policies,
    a: &RequestAttrs<'_>,
    scope: Scope<'_>,
) -> Result<Vec<Applicable<'p>>, MissingAttribute> {
    let org_group = p.organization_uid(a.organization);
    let mut out = Vec::new();
    for (level, owner) in owners(a, scope) {
        for d in p.owned(level, owner) {
            let (partition, matching, exclude, declared) = match &d.kind {
                DeclKind::Limit(l) => {
                    if !p.limits.enabled {
                        continue;
                    }
                    (&l.partition, &l.matching, &l.exclude, u128::from(l.limit))
                }
                DeclKind::Budget(b) => (&b.partition, &b.matching, &b.exclude, b.amount),
            };
            if matching.as_ref().is_some_and(|m| !a.matches(m))
                || exclude.as_ref().is_some_and(|m| a.matches(m))
            {
                continue;
            }
            let Some(values) = a.partition(partition)? else {
                continue;
            };
            let quota = match values.as_slice() {
                [(_, v)] => partition.overrides.get(v).copied().unwrap_or(declared),
                _ => declared,
            };
            let values = scoped(values, Some(a.project));
            let kind = match d.kind {
                DeclKind::Limit(_) => "limit",
                DeclKind::Budget(_) => "budget",
            };
            let node_path = if d.level == Level::Node {
                d.owner.as_str()
            } else {
                ""
            };
            let counter = CounterId::derive(
                kind,
                d.level,
                d.owner_uid,
                node_path,
                &d.name,
                &window_key(&d.kind),
                &values,
            );
            let identity = CounterId::derive(kind, d.level, d.owner_uid, node_path, &d.name, "", &values);
            out.push(Applicable {
                decl: d,
                counter,
                quota,
                group: if d.level.is_tenant() { org_group } else { 0 },
                partition: values,
                identity,
            });
        }
    }
    Ok(out)
}

/// The declarations of a key's tenancy levels (`levels`, among gateway, organization, team, project
/// and key) resolved to the counters its own requests use, whatever the declarations' filters: the
/// self-service view. A partition attribute the caller lacks is its shared value; a declaration that
/// exempts or refuses such requests is left out.
#[must_use]
pub fn standing<'p>(p: &'p Policies, a: &RequestAttrs<'_>, levels: &[Level]) -> Vec<Applicable<'p>> {
    let org_group = p.organization_uid(a.organization);
    let mut out = Vec::new();
    for (level, owner) in owners(a, Scope::Request) {
        if !levels.contains(&level) {
            continue;
        }
        for d in p.owned(level, owner) {
            let (partition, declared) = match &d.kind {
                DeclKind::Limit(l) if p.limits.enabled => (&l.partition, u128::from(l.limit)),
                DeclKind::Limit(_) => continue,
                DeclKind::Budget(b) => (&b.partition, b.amount),
            };
            let Ok(Some(values)) = a.partition(partition) else {
                continue;
            };
            let quota = match values.as_slice() {
                [(_, v)] => partition.overrides.get(v).copied().unwrap_or(declared),
                _ => declared,
            };
            let values = scoped(values, Some(a.project));
            let kind = match d.kind {
                DeclKind::Limit(_) => "limit",
                DeclKind::Budget(_) => "budget",
            };
            out.push(Applicable {
                decl: d,
                counter: CounterId::derive(
                    kind,
                    d.level,
                    d.owner_uid,
                    "",
                    &d.name,
                    &window_key(&d.kind),
                    &values,
                ),
                quota,
                group: if d.level.is_tenant() { org_group } else { 0 },
                identity: CounterId::derive(kind, d.level, d.owner_uid, "", &d.name, "", &values),
                partition: values,
            });
        }
    }
    out
}

/// Partition values as counters key on them: an end user is scoped to the project it belongs to
/// (`<project>/<end user>`), so the same end-user id in two projects is two end users. Values that
/// are already scoped, and the shared value of requests without one, are kept.
fn scoped(values: Vec<(String, String)>, project: Option<&str>) -> Vec<(String, String)> {
    values
        .into_iter()
        .map(|(attr, v)| match project {
            Some(p) if attr == "end_user" && v != SHARED_VALUE && !v.contains('/') => {
                let v = format!("{p}/{v}");
                (attr, v)
            }
            _ => (attr, v),
        })
        .collect()
}

/// The value an override is keyed by: an end user without its project.
fn unscoped<'v>(attr: &str, v: &'v str) -> &'v str {
    match v.split_once('/') {
        Some((_, rest)) if attr == "end_user" => rest,
        _ => v,
    }
}

/// A budget counter named by its declaration and partition values, as the Admin API names it.
#[derive(Clone, Debug)]
pub struct NamedBudget<'p> {
    pub decl: &'p Declared,
    pub budget: &'p crate::decl::BudgetDecl,
    pub counter: CounterId,
    pub identity: CounterId,
    /// The quota for this partition: an override, or the declared amount.
    pub quota: u128,
    /// The partition values in declaration order (absent ones as the shared value).
    pub partition: Vec<(String, String)>,
}

/// The value a partition attribute takes for requests that lack it (`on_missing: shared`).
pub const SHARED_VALUE: &str = "\u{0}";

/// Finds the budget `name` declared by `(level, owner)` and resolves its counter for the given
/// partition values (attribute name to value), an end user of `project` when it is not scoped yet;
/// `None` when there is no such budget or a value names an attribute the budget does not partition
/// on.
#[must_use]
pub fn named_budget<'p>(
    p: &'p Policies,
    level: Level,
    owner: &str,
    name: &str,
    values: &[(String, String)],
    project: Option<&str>,
) -> Option<NamedBudget<'p>> {
    let decl = p.owned(level, owner).find(|d| d.name == name)?;
    let DeclKind::Budget(budget) = &decl.kind else {
        return None;
    };
    let attrs: Vec<String> = budget.partition.attrs.iter().map(PartitionAttr::name).collect();
    if values.iter().any(|(a, _)| !attrs.contains(a)) {
        return None;
    }
    let partition: Vec<(String, String)> = attrs
        .into_iter()
        .map(|a| {
            let v = values
                .iter()
                .find(|(k, _)| *k == a)
                .map_or_else(|| SHARED_VALUE.to_owned(), |(_, v)| v.clone());
            (a, v)
        })
        .collect();
    let quota = match partition.as_slice() {
        [(a, v)] => budget
            .partition
            .overrides
            .get(unscoped(a, v))
            .copied()
            .unwrap_or(budget.amount),
        _ => budget.amount,
    };
    let partition = scoped(partition, project);
    let node_path = if level == Level::Node { owner } else { "" };
    let counter = CounterId::derive(
        "budget",
        level,
        decl.owner_uid,
        node_path,
        name,
        &window_key(&decl.kind),
        &partition,
    );
    let identity = CounterId::derive("budget", level, decl.owner_uid, node_path, name, "", &partition);
    Some(NamedBudget {
        decl,
        budget,
        counter,
        identity,
        quota,
        partition,
    })
}

/// Finds the rate or concurrency limit `name` declared by `(level, owner)` and resolves the counter
/// that requests of `organization` with the given partition values use, as the Admin API reads it;
/// `None` when there is no such limit or a value names an attribute it does not partition on.
#[must_use]
pub fn named_limit<'p>(
    p: &'p Policies,
    level: Level,
    owner: &str,
    name: &str,
    values: &[(String, String)],
    project: Option<&str>,
    organization: &str,
) -> Option<Applicable<'p>> {
    let decl = p.owned(level, owner).find(|d| d.name == name)?;
    let DeclKind::Limit(limit) = &decl.kind else {
        return None;
    };
    let attrs: Vec<String> = limit.partition.attrs.iter().map(PartitionAttr::name).collect();
    if values.iter().any(|(a, _)| !attrs.contains(a)) {
        return None;
    }
    let partition: Vec<(String, String)> = attrs
        .into_iter()
        .map(|a| {
            let v = values
                .iter()
                .find(|(k, _)| *k == a)
                .map_or_else(|| SHARED_VALUE.to_owned(), |(_, v)| v.clone());
            (a, v)
        })
        .collect();
    let declared = u128::from(limit.limit);
    let quota = match partition.as_slice() {
        [(a, v)] => limit
            .partition
            .overrides
            .get(unscoped(a, v))
            .copied()
            .unwrap_or(declared),
        _ => declared,
    };
    let partition = scoped(partition, project);
    let node_path = if level == Level::Node { owner } else { "" };
    let counter = CounterId::derive(
        "limit",
        level,
        decl.owner_uid,
        node_path,
        name,
        &window_key(&decl.kind),
        &partition,
    );
    let identity = CounterId::derive("limit", level, decl.owner_uid, node_path, name, "", &partition);
    Some(Applicable {
        decl,
        counter,
        quota,
        group: if level.is_tenant() {
            p.organization_uid(organization)
        } else {
            0
        },
        partition,
        identity,
    })
}

#[cfg(test)]
mod tests {
    use antirion_core::ResourceUid;
    use serde_json::json;

    use super::*;

    fn policies(doc: serde_json::Value) -> Policies {
        Policies::compile(&doc, &|_, n| ResourceUid(n.len() as u128 + 1)).unwrap()
    }

    fn attrs<'a>(key: &'a str, model: &'a str, tags: &'a [String]) -> RequestAttrs<'a> {
        RequestAttrs {
            organization: "default",
            team: "default",
            project: "default",
            key,
            model: Some(model),
            endpoint: "chat_completions",
            tags,
            ..RequestAttrs::default()
        }
    }

    #[test]
    fn globs() {
        assert!(glob("claude-*", "claude-sonnet"));
        assert!(glob("*", ""));
        assert!(glob("a*b*c", "aXbYc"));
        assert!(!glob("a*b*c", "aXbY"));
        assert!(glob("gpt-5", "gpt-5"));
        assert!(!glob("gpt-5", "gpt-50"));
        assert!(glob("*-mini", "gpt-4o-mini"));
    }

    #[test]
    fn filters_and_levels() {
        let p = policies(json!({
            "policies": {"limits": {"g": {"unit": "requests", "limit": 1, "per": "1h", "exclude": {"models": ["free"]}}}},
            "keys": {"k": {"policies": {"limits": {"t": {"unit": "tokens", "limit": 5, "per": "1h", "match": {"tags": ["env:*"]}}}}}},
            "models": {"m": {"route": "r", "policies": {"budgets": {"b": {"unit": "usd", "amount": "1"}}}}}
        }));
        let none: Vec<String> = vec![];
        let prod = vec!["env:prod".to_owned()];
        assert_eq!(
            applicable(&p, &attrs("k", "free", &none), Scope::Request)
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            applicable(&p, &attrs("k", "m", &none), Scope::Request)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            applicable(&p, &attrs("k", "m", &prod), Scope::Request)
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            applicable(&p, &attrs("other", "m", &prod), Scope::Request)
                .unwrap()
                .len(),
            2
        );
        let off = policies(json!({"limits": {"enabled": false},
            "policies": {"limits": {"g": {"unit": "requests", "limit": 1, "per": "1h"}}, "budgets": {"b": {"unit": "usd", "amount": "1"}}}}));
        let set = applicable(&off, &attrs("k", "m", &none), Scope::Request).unwrap();
        assert_eq!(set.len(), 1);
        assert!(matches!(set[0].decl.kind, DeclKind::Budget(_)));
    }

    #[test]
    fn partitions_overrides_and_missing() {
        let doc = |on_missing: &str| {
            json!({"organizations": {"default": {"policies": {"limits": {"eu": {"unit": "requests", "limit": 2, "per": "1h",
                "partition_by": ["end_user"], "overrides": {"vip": 5}, "on_missing": on_missing}}}}}})
        };
        let p = policies(doc("shared"));
        let none: Vec<String> = vec![];
        let mut a = attrs("k", "m", &none);
        a.end_user = Some("alice");
        let alice = applicable(&p, &a, Scope::Request).unwrap();
        a.end_user = Some("vip");
        let vip = applicable(&p, &a, Scope::Request).unwrap();
        assert_ne!(alice[0].counter, vip[0].counter);
        assert_eq!((alice[0].quota, vip[0].quota), (2, 5));
        assert_eq!(alice[0].group, p.organization_uid("default"));
        a.end_user = None;
        let shared = applicable(&p, &a, Scope::Request).unwrap();
        assert_eq!(shared.len(), 1);
        assert_ne!(shared[0].counter, alice[0].counter);
        assert!(
            applicable(&policies(doc("skip")), &a, Scope::Request)
                .unwrap()
                .is_empty()
        );
        let err = applicable(&policies(doc("reject")), &a, Scope::Request).unwrap_err();
        assert_eq!(err.attribute, "end_user");
        a.end_user = Some("alice");
        a.project = "other";
        let elsewhere = applicable(&p, &a, Scope::Request).unwrap();
        assert_ne!(
            elsewhere[0].counter, alice[0].counter,
            "end users are scoped to their project"
        );
        assert_eq!(
            alice[0].partition,
            [("end_user".to_owned(), "default/alice".to_owned())]
        );
        let named = named_budget_doc();
        let b = named_budget(
            &named,
            Level::Key,
            "k",
            "b",
            &[("end_user".into(), "vip".into())],
            Some("p"),
        )
        .unwrap();
        assert_eq!(b.quota, 5);
        assert_eq!(b.partition, [("end_user".to_owned(), "p/vip".to_owned())]);
        let again = named_budget(&named, Level::Key, "k", "b", &b.partition, None).unwrap();
        assert_eq!((again.counter, again.quota), (b.counter, 5));
    }

    fn named_budget_doc() -> Policies {
        policies(
            json!({"keys": {"k": {"policies": {"budgets": {"b": {"unit": "tokens", "amount": 10,
            "partition_by": ["end_user"], "overrides": {"vip": 5}}}}}}}),
        )
    }

    #[test]
    fn ip_and_tag_partitions() {
        let tags = vec!["team:blue".to_owned(), "env".to_owned()];
        let mut a = attrs("k", "m", &tags);
        assert_eq!(a.value(&PartitionAttr::Tag("team".into())), Some("blue".into()));
        assert_eq!(a.value(&PartitionAttr::Tag("env".into())), None);
        a.ip = Some("2001:db8:1:2:3:4:5:6".parse().unwrap());
        assert_eq!(a.value(&PartitionAttr::Ip), Some("2001:db8:1:2::/64".into()));
        a.ip = Some("10.1.2.3".parse().unwrap());
        assert_eq!(a.value(&PartitionAttr::Ip), Some("10.1.2.3".into()));
    }

    #[test]
    fn attempt_scope_and_window_identity() {
        let p = policies(json!({
            "routes": {"r": {"tree": {"type": "fallback", "children": [{"deployment": "d", "policies": {"limits": {"n": {"unit": "requests", "limit": 1, "per": "1m"}}}}]}}},
            "deployments": {"d": {"provider": "p", "model": "x", "limits": {"rpm": {"unit": "requests", "limit": 2, "per": "1h"}}}}
        }));
        let none: Vec<String> = vec![];
        let a = attrs("k", "m", &none);
        assert!(applicable(&p, &a, Scope::Request).unwrap().is_empty());
        let nodes = vec!["r/d".to_owned()];
        let set = applicable(
            &p,
            &a,
            Scope::Attempt {
                nodes: &nodes,
                deployment: Some("d"),
            },
        )
        .unwrap();
        assert_eq!(set.len(), 2);
        let p2 = policies(
            json!({"keys": {"k": {"policies": {"limits": {"x": {"unit": "requests", "limit": 1, "per": "1m"}}}}}}),
        );
        let p3 = policies(
            json!({"keys": {"k": {"policies": {"limits": {"x": {"unit": "requests", "limit": 9, "per": "1m"}}}}}}),
        );
        let p4 = policies(
            json!({"keys": {"k": {"policies": {"limits": {"x": {"unit": "requests", "limit": 1, "per": "2m"}}}}}}),
        );
        let p5 = policies(json!({"keys": {"k": {"policies": {"limits": {"x": {
            "unit": "requests", "limit": 1, "per": "1m", "algorithm": "sliding_window"}}}}}}));
        let id = |p: &Policies| applicable(p, &a, Scope::Request).unwrap()[0].counter;
        assert_eq!(id(&p2), id(&p3));
        assert_eq!(
            id(&p2),
            id(&p4),
            "a GCRA counter keeps its identity across a changed window"
        );
        assert_ne!(id(&p2), id(&p5));
    }
}

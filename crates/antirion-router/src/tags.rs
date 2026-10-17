//! Tag filtering of a route's leaves by the request's tags: positive tags pick the leaves that
//! carry one of them (else the leaves tagged with the default tag), then leaves with an excluded tag
//! (`!x`) and leaves missing a required tag (`&x`) are removed.

use std::collections::HashSet;

use crate::tree::CompiledRoute;

/// The request's tags, split by kind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestTags<'a> {
    pub positive: Vec<&'a str>,
    pub excluded: Vec<&'a str>,
    pub required: Vec<&'a str>,
}

impl<'a> RequestTags<'a> {
    /// Splits tags as written (`x`, `!x`, `&x`).
    #[must_use]
    pub fn parse(tags: impl IntoIterator<Item = &'a str>) -> RequestTags<'a> {
        let mut out = RequestTags::default();
        for t in tags {
            if let Some(x) = t.strip_prefix('!') {
                out.excluded.push(x);
            } else if let Some(x) = t.strip_prefix('&') {
                out.required.push(x);
            } else {
                out.positive.push(t);
            }
        }
        out
    }
}

/// The leaves of `route` the tags allow, by node index.
#[must_use]
pub fn allowed(route: &CompiledRoute, tags: &RequestTags<'_>, default_tag: &str) -> HashSet<usize> {
    let leaves: Vec<(usize, &[String])> = route
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.kind.is_leaf())
        .map(|(i, n)| (i, n.tags.as_slice()))
        .collect();
    let has = |carried: &[String], t: &str| carried.iter().any(|c| c == t);
    let matched: Vec<usize> = leaves
        .iter()
        .filter(|(_, carried)| tags.positive.iter().any(|t| has(carried, t)))
        .map(|(i, _)| *i)
        .collect();
    let candidates = if matched.is_empty() {
        leaves
            .iter()
            .filter(|(_, carried)| has(carried, default_tag))
            .map(|(i, _)| *i)
            .collect()
    } else {
        matched
    };
    candidates
        .into_iter()
        .filter(|i| {
            let carried = leaves.iter().find(|(j, _)| j == i).map_or(&[][..], |(_, c)| *c);
            !tags.excluded.iter().any(|t| has(carried, t)) && tags.required.iter().all(|t| has(carried, t))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn route() -> CompiledRoute {
        let doc = json!({
            "deployments": {"x": {}, "y": {}, "z": {}},
            "routes": {"r": {"tree": {"type": "fallback", "children": [
                {"deployment": "x", "tags": ["free"]},
                {"deployment": "y", "tags": ["paid", "eu"]},
                {"deployment": "z", "tags": ["default"]}
            ]}}}
        });
        let s = crate::compile(&doc).unwrap();
        s.routes.get("r").unwrap().tree.clone()
    }

    fn names(r: &CompiledRoute, set: &HashSet<usize>) -> Vec<String> {
        let mut v: Vec<String> = set
            .iter()
            .filter_map(|i| r.node(*i))
            .map(|n| n.path.rsplit('/').next().unwrap_or_default().to_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn positive_tags_then_the_default_pool_then_exclusions() {
        let r = route();
        let pick = |tags: &[&str]| {
            names(
                &r,
                &allowed(&r, &RequestTags::parse(tags.iter().copied()), "default"),
            )
        };
        assert_eq!(pick(&["free"]), ["0"]);
        assert_eq!(pick(&["paid"]), ["1"]);
        assert_eq!(pick(&[]), ["2"]);
        assert_eq!(pick(&["unknown"]), ["2"]);
        assert_eq!(pick(&["!paid"]), ["2"]);
        assert_eq!(pick(&["paid", "&eu"]), ["1"]);
        assert!(pick(&["free", "&eu"]).is_empty());
        assert!(pick(&["!default"]).is_empty());
    }
}

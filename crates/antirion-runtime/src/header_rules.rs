//! The `headers` member of every `policies` block, compiled per level, and the rules in effect for
//! one request.

use std::collections::HashMap;

use antirion_config::{ConfigError, DocPath};
use antirion_egress::headers::{HeaderBlock, HeaderRules};
use serde_json::Value;

/// Header blocks by level, broadest first.
#[derive(Clone, Debug, Default)]
pub struct HeaderTable {
    gateway: Option<HeaderBlock>,
    organizations: HashMap<String, HeaderBlock>,
    teams: HashMap<String, HeaderBlock>,
    projects: HashMap<String, HeaderBlock>,
    keys: HashMap<String, HeaderBlock>,
    routes: HashMap<String, HeaderBlock>,
    models: HashMap<String, HeaderBlock>,
}

/// Where a request sits in the policy hierarchy.
#[derive(Clone, Copy, Debug)]
pub struct Levels<'a> {
    pub organization: &'a str,
    pub team: &'a str,
    pub project: &'a str,
    pub key: &'a str,
    pub route: Option<&'a str>,
    pub model: &'a str,
}

impl HeaderTable {
    pub fn compile(doc: &Value) -> Result<HeaderTable, Vec<ConfigError>> {
        let root = DocPath::root();
        let mut errors = Vec::new();
        let mut block = |v: &Value, at: DocPath| -> Option<HeaderBlock> {
            let h = v.get("policies")?.get("headers")?;
            antirion_egress::config::header_block(h, &at.key("policies").key("headers"))
                .map_err(|e| errors.extend(e))
                .ok()
        };
        let mut table = HeaderTable {
            gateway: block(doc, root.clone()),
            ..HeaderTable::default()
        };
        for (level, map) in [
            ("organizations", &mut table.organizations),
            ("teams", &mut table.teams),
            ("projects", &mut table.projects),
            ("keys", &mut table.keys),
            ("routes", &mut table.routes),
            ("models", &mut table.models),
        ] {
            for (name, r) in doc.get(level).and_then(Value::as_object).into_iter().flatten() {
                if let Some(b) = block(r, root.key(level).key(name)) {
                    map.insert(name.clone(), b);
                }
            }
        }
        if errors.is_empty() { Ok(table) } else { Err(errors) }
    }

    /// The rules for one request: gateway, organization, team, project, key, route, model.
    #[must_use]
    pub fn rules(&self, l: &Levels<'_>) -> HeaderRules {
        let chain = [
            self.gateway.as_ref(),
            self.organizations.get(l.organization),
            self.teams.get(l.team),
            self.projects.get(l.project),
            self.keys.get(l.key),
            l.route.and_then(|r| self.routes.get(r)),
            self.models.get(l.model),
        ];
        HeaderRules::merge(chain.into_iter().flatten())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn levels_merge_broadest_first() {
        let doc = json!({
            "policies": {"headers": {"forward": ["x-a", "x-b"], "deny": ["x-b"]}},
            "models": {"m": {"policies": {"headers": {"forward": ["x-b", "x-c"]}}}},
            "keys": {"k": {"policies": {"headers": {"deny": ["x-c"]}}}}
        });
        let t = HeaderTable::compile(&doc).unwrap();
        let l = Levels {
            organization: "default",
            team: "default",
            project: "default",
            key: "other",
            route: None,
            model: "m",
        };
        let r = t.rules(&l);
        assert_eq!(r.forward.len(), 2);
        assert_eq!(r.deny.len(), 1);
        let with_key = t.rules(&Levels { key: "k", ..l });
        assert_eq!(with_key.deny.len(), 2);
        let bad = json!({"teams": {"t": {"policies": {"headers": {"forward": ["cookie"]}}}}});
        let errors = HeaderTable::compile(&bad).unwrap_err();
        assert_eq!(errors[0].path, "teams.t.policies.headers.forward[0]");
    }
}

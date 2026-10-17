//! Public Models: exact names, aliases (with their params) and wildcard patterns, resolved to the
//! route that serves them.

use std::cmp::Ordering;

use antirion_core::UtcMicros;
use regex::Regex;
use serde_json::Value;

use crate::params::Params;
use crate::plan::Captures;

/// A compiled public Model.
#[derive(Clone, Debug)]
pub struct ModelRt {
    pub name: String,
    pub enabled: bool,
    /// `visibility: internal` Models are only reachable from route trees.
    pub public: bool,
    pub owned_by: String,
    /// The route that serves the Model: a named route, or the Model's anonymous route (its name).
    pub route: String,
    /// Whether the Model names a route (rather than an inline target).
    pub named_route: bool,
    /// Alias names, in name order.
    pub aliases: Vec<String>,
    pub wildcard: bool,
    /// The admin-set tier label.
    pub tier: Option<String>,
    pub labels: Vec<String>,
    pub lifecycle: Lifecycle,
    /// Who may use the Model while it is `preview` or `restricted`.
    pub access: ModelAccess,
}

/// The tiers every gateway has without declaring them.
pub const BUILTIN_TIERS: [&str; 4] = ["frontier", "balanced", "fast", "specialized"];

/// A Model's lifecycle state.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum LifecycleState {
    Preview,
    #[default]
    Ga,
    Restricted,
    Deprecated,
}

impl LifecycleState {
    #[must_use]
    pub fn parse(s: &str) -> Option<LifecycleState> {
        Some(match s {
            "preview" => LifecycleState::Preview,
            "ga" => LifecycleState::Ga,
            "restricted" => LifecycleState::Restricted,
            "deprecated" => LifecycleState::Deprecated,
            _ => return None,
        })
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            LifecycleState::Preview => "preview",
            LifecycleState::Ga => "ga",
            LifecycleState::Restricted => "restricted",
            LifecycleState::Deprecated => "deprecated",
        }
    }
}

/// A Model's lifecycle: its state, end of life and successor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lifecycle {
    pub state: LifecycleState,
    pub eol_at: Option<UtcMicros>,
    pub replacement: Option<String>,
    /// When the Model entered its current state.
    pub since: Option<UtcMicros>,
}

impl Lifecycle {
    pub(crate) fn from_value(v: Option<&Value>) -> Lifecycle {
        let v = v.unwrap_or(&Value::Null);
        Lifecycle {
            state: v
                .get("state")
                .and_then(Value::as_str)
                .and_then(LifecycleState::parse)
                .unwrap_or_default(),
            eol_at: v
                .get("eol_at")
                .and_then(Value::as_str)
                .and_then(|t| UtcMicros::parse_rfc3339(t).ok()),
            replacement: v.get("replacement").and_then(Value::as_str).map(str::to_owned),
            since: None,
        }
    }

    /// The end of life has passed: the Model and its aliases no longer resolve.
    #[must_use]
    pub fn retired(&self, now: UtcMicros) -> bool {
        self.eol_at.is_some_and(|eol| now >= eol)
    }

    /// Only the principals in the Model's access list may use it.
    #[must_use]
    pub fn gated(&self) -> bool {
        matches!(self.state, LifecycleState::Preview | LifecycleState::Restricted)
    }

    /// The state a reader sees: `retired` once the end of life has passed.
    #[must_use]
    pub fn shown(&self, now: UtcMicros) -> &'static str {
        if self.retired(now) {
            "retired"
        } else {
            self.state.as_str()
        }
    }
}

/// The organizations, teams, projects and keys a gated Model admits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelAccess {
    pub organizations: Vec<String>,
    pub teams: Vec<String>,
    pub projects: Vec<String>,
    pub keys: Vec<String>,
}

/// A caller's place in the tenancy, for access lists.
#[derive(Copy, Clone, Debug)]
pub struct Caller<'a> {
    pub organization: &'a str,
    pub team: &'a str,
    pub project: &'a str,
    pub key: &'a str,
}

impl ModelAccess {
    pub(crate) fn from_value(v: Option<&Value>) -> ModelAccess {
        let list = |k: &str| -> Vec<String> {
            v.and_then(|v| v.get(k))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        };
        ModelAccess {
            organizations: list("organizations"),
            teams: list("teams"),
            projects: list("projects"),
            keys: list("keys"),
        }
    }

    /// The caller's key or one of its ancestors is listed.
    #[must_use]
    pub fn admits(&self, c: Caller<'_>) -> bool {
        let has = |l: &[String], v: &str| l.iter().any(|x| x == v);
        has(&self.keys, c.key)
            || has(&self.projects, c.project)
            || has(&self.teams, c.team)
            || has(&self.organizations, c.organization)
    }
}

impl ModelRt {
    /// Whether the caller may use the Model under its lifecycle state.
    #[must_use]
    pub fn admits(&self, c: Caller<'_>) -> bool {
        !self.lifecycle.gated() || self.access.admits(c)
    }
}

/// A wildcard pattern, compiled to an anchored regex.
#[derive(Clone, Debug)]
pub(crate) struct Wildcard {
    pub(crate) pattern: String,
    pub(crate) model: String,
    regex: Regex,
    literals: usize,
    stars: usize,
}

impl Wildcard {
    pub(crate) fn new(pattern: &str, model: &str) -> Option<Wildcard> {
        let source: String = pattern
            .split('*')
            .map(regex::escape)
            .collect::<Vec<_>>()
            .join("(.*)");
        Some(Wildcard {
            pattern: pattern.to_owned(),
            model: model.to_owned(),
            regex: Regex::new(&format!("^{source}$")).ok()?,
            literals: pattern.chars().filter(|c| *c != '*').count(),
            stars: pattern.matches('*').count(),
        })
    }

    pub(crate) fn captures(&self, name: &str) -> Option<Captures> {
        let caps = self.regex.captures(name)?;
        Some(Captures {
            model: name.to_owned(),
            groups: caps
                .iter()
                .skip(1)
                .map(|m| m.map_or_else(String::new, |m| m.as_str().to_owned()))
                .collect(),
        })
    }

    /// Most specific first: more literal characters, then fewer `*`, then byte order.
    pub(crate) fn specificity(a: &Wildcard, b: &Wildcard) -> Ordering {
        b.literals
            .cmp(&a.literals)
            .then(a.stars.cmp(&b.stars))
            .then(a.pattern.as_bytes().cmp(b.pattern.as_bytes()))
    }
}

/// How a requested name resolved.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub model: std::sync::Arc<ModelRt>,
    /// The alias's params, when the name was an alias.
    pub alias_params: Option<Params>,
    pub captures: Option<Captures>,
    /// `exact`, `alias` or `wildcard`.
    pub via: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_and_access() {
        let l = Lifecycle::from_value(Some(&serde_json::json!({
            "state": "restricted", "eol_at": "2026-01-01T00:00:00Z"
        })));
        assert_eq!(l.state, LifecycleState::Restricted);
        assert!(l.gated());
        let eol = UtcMicros::parse_rfc3339("2026-01-01T00:00:00Z").unwrap();
        assert!(!l.retired(UtcMicros(eol.0 - 1)));
        assert!(l.retired(eol));
        assert_eq!(l.shown(eol), "retired");
        let a = ModelAccess::from_value(Some(&serde_json::json!({"teams": ["t1"]})));
        let caller = |team| Caller {
            organization: "o",
            team,
            project: "p",
            key: "k",
        };
        assert!(a.admits(caller("t1")));
        assert!(!a.admits(caller("t2")));
        assert_eq!(Lifecycle::from_value(None).state, LifecycleState::Ga);
    }

    #[test]
    fn specificity_and_captures() {
        let mut w = [
            Wildcard::new("openai/*", "d1").unwrap(),
            Wildcard::new("*", "d3").unwrap(),
            Wildcard::new("openai/gpt-*", "d2").unwrap(),
        ];
        w.sort_by(Wildcard::specificity);
        let first = |name: &str| {
            w.iter()
                .find(|x| x.captures(name).is_some())
                .map(|x| x.model.clone())
        };
        assert_eq!(first("openai/gpt-4o").as_deref(), Some("d2"));
        assert_eq!(first("openai/o3").as_deref(), Some("d1"));
        assert_eq!(first("claude").as_deref(), Some("d3"));
        let c = Wildcard::new("cap/*", "m").unwrap().captures("cap/a/b").unwrap();
        assert_eq!(c.render("{1}|{model}"), "a/b|cap/a/b");
    }
}

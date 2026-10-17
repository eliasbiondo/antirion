//! The Models a request may move to beyond its own route: request-supplied fallbacks
//! (`x-antirion-fallbacks`), whose tiers follow the route's, and class fallbacks
//! (`retry.class_fallbacks`), whose tiers replace what is left of the plan after a context-length
//! or content-policy verdict, once per request. Each is resolved when the plan is built.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use antirion_router::{Captures, Params, RouteRt};
use antirion_runtime::Snapshot;
use antirion_runtime::identity::KeyRt;
use serde_json::Value;

use crate::authorize::{self, Target};
use crate::infer::model::resolution;
use crate::policy::Levels;

/// The tiers of another Model, with what positions in them need.
#[derive(Debug)]
pub(crate) struct Segment {
    pub(crate) model: String,
    pub(crate) route: Arc<RouteRt>,
    pub(crate) alias: Option<Params>,
    pub(crate) captures: Option<Captures>,
    /// The merged resilience policy above the Model's route nodes.
    pub(crate) base: Value,
    /// The transition label of the move into the Model.
    pub(crate) label: String,
}

/// A Model name that cannot be moved to, and why (the skip reason).
pub(crate) type Refused = &'static str;

/// The Models a request may move to beyond its route.
#[derive(Debug, Default)]
pub(crate) struct Fallbacks {
    /// The request's fallbacks in order: each name with its tiers, or the reason it is skipped.
    pub(crate) appended: Vec<(String, Result<Segment, Refused>)>,
    class: HashMap<String, Result<Segment, Refused>>,
    taken: AtomicBool,
}

impl Fallbacks {
    /// Resolves the request's fallbacks and the class fallbacks its policies name, for `key`.
    pub(crate) fn new(
        snap: &Snapshot,
        key: &KeyRt,
        requested: &[String],
        class_names: &[String],
    ) -> Fallbacks {
        let appended = requested
            .iter()
            .map(|n| (n.clone(), segment(snap, key, n, "fallbacks")))
            .collect();
        let class = class_names
            .iter()
            .map(|n| (n.clone(), segment(snap, key, n, "class_fallback")))
            .collect();
        Fallbacks {
            appended,
            class,
            taken: AtomicBool::new(false),
        }
    }

    /// The tiers of class fallback Model `model`, when the key may use it.
    pub(crate) fn class(&self, model: &str) -> Option<&Segment> {
        self.class.get(model).and_then(|r| r.as_ref().ok())
    }

    /// The request moved to a class fallback; there is no second one.
    pub(crate) fn take(&self) {
        self.taken.store(true, Ordering::Relaxed);
    }

    pub(crate) fn taken(&self) -> bool {
        self.taken.load(Ordering::Relaxed)
    }
}

/// The tiers of Model `name` for `key`: skipped as `unknown` when no Model has that name, as
/// `unauthorized` when the key may not use it.
fn segment(snap: &Snapshot, key: &KeyRt, name: &str, kind: &str) -> Result<Segment, Refused> {
    if snap.routing.resolve(name).is_none() {
        return Err("unknown");
    }
    let target = authorize::resolve(snap, key, name).map_err(|_| "unauthorized")?;
    if !matches!(target, Target::Model(_)) {
        return Err("unknown");
    }
    let r = resolution(snap, name, &target).map_err(|_| "unknown")?;
    let base = Levels::new(snap, key, &r.model_name, r.policy_route.as_deref()).effective("resilience");
    Ok(Segment {
        label: format!("{kind}@{}", r.model_name),
        model: r.model_name,
        route: r.route,
        alias: r.alias,
        captures: r.captures,
        base,
    })
}

/// The class fallback Models named by the request's policy or by any node of its route.
pub(crate) fn class_names(base: &Value, route: &RouteRt) -> Vec<String> {
    let blocks = std::iter::once(base).chain(
        route
            .tree
            .nodes
            .iter()
            .filter_map(|n| n.policies.as_ref())
            .filter_map(|p| p.get("resilience")),
    );
    let mut out: Vec<String> = blocks
        .filter_map(|b| b.pointer("/retry/class_fallbacks").and_then(Value::as_object))
        .flat_map(|m| m.values().filter_map(Value::as_str).map(str::to_owned))
        .collect();
    out.sort();
    out.dedup();
    out
}

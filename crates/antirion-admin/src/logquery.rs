//! The request-log selection every log and analytics endpoint accepts: equality filters (repeat a
//! parameter for OR), status classes, latency and cost ranges, cache outcome, streaming, guardrail
//! facts, `q`, `metadata.<name>` and the time range; the same selection on the rollups when they can
//! answer it; and the caller's scope.

use antirion_core::UtcMicros;
use antirion_identity::rbac::{AdminPrincipal, Scope};
use antirion_limits::money::Nusd;
use antirion_runtime::reqlog::analytics::{Dim, RollupFilter};
use antirion_store::logs::{Arg, LogColumn, LogFilter, ROLLUP_DIMS, ScopeRef};

use crate::call::Call;
use crate::error::AdminError;

/// Equality filters: parameter, column, and the rollup column when rollups carry it.
const EQUALS: &[(&str, LogColumn, Option<&str>)] = &[
    ("request_id", LogColumn::RequestId, None),
    ("organization", LogColumn::Organization, Some("organization")),
    ("team", LogColumn::Team, Some("team")),
    ("project", LogColumn::Project, Some("project")),
    ("key", LogColumn::Key, Some("key_name")),
    ("model", LogColumn::Model, Some("model")),
    ("provider", LogColumn::Provider, Some("provider")),
    ("deployment", LogColumn::Deployment, Some("deployment")),
    ("status_code", LogColumn::StatusCode, Some("status_code")),
    ("code", LogColumn::Code, Some("code")),
    ("response_path", LogColumn::ResponsePath, Some("response_path")),
    ("end_user", LogColumn::EndUser, None),
    ("session_id", LogColumn::SessionId, None),
    ("operation", LogColumn::Operation, None),
    (
        "requested_model",
        LogColumn::RequestedModel,
        Some("requested_model"),
    ),
    ("user", LogColumn::User, Some("user_name")),
    ("environment", LogColumn::Environment, Some("environment")),
    ("cache", LogColumn::Cache, Some("cache_status")),
];

/// Every selection parameter (besides `metadata.<name>`).
pub(crate) const SELECTION: &[&str] = &[
    "from",
    "to",
    "request_id",
    "organization",
    "team",
    "project",
    "key",
    "model",
    "provider",
    "deployment",
    "status_code",
    "code",
    "response_path",
    "end_user",
    "session_id",
    "operation",
    "tag",
    "status_class",
    "latency_ms_min",
    "latency_ms_max",
    "cost_usd_min",
    "cost_usd_max",
    "cache",
    "stream",
    "guardrail",
    "guardrail_verdict",
    "guardrail_category",
    "flagged",
    "requested_model",
    "user",
    "environment",
    "q",
];

/// The filters of the tail: equality on record members and `metadata.<name>`.
pub(crate) const TAIL_FILTERS: &[&str] = &[
    "from",
    "to",
    "request_id",
    "organization",
    "team",
    "project",
    "key",
    "model",
    "provider",
    "deployment",
    "status_code",
    "code",
    "response_path",
    "end_user",
    "session_id",
    "tag",
];

const CACHE_VALUES: [&str; 4] = ["hit", "miss", "semantic_hit", "bypass"];

/// A parsed selection.
#[derive(Clone, Debug, Default)]
pub(crate) struct Selection {
    pub(crate) filter: LogFilter,
    /// `None` when a filter only records can answer.
    pub(crate) rollup: Option<RollupFilter>,
    pub(crate) metadata: Vec<(String, String)>,
}

/// Refuses any parameter outside `extra` and the selection.
pub(crate) fn allow(call: &Call, selection: &[&str], extra: &[&str]) -> Result<(), AdminError> {
    match call.query.iter().find(|(k, _)| {
        !k.starts_with("metadata.") && !selection.contains(&k.as_str()) && !extra.contains(&k.as_str())
    }) {
        Some((k, _)) => Err(AdminError::invalid_parameter(format!(
            "unknown query parameter {k:?}"
        ))),
        None => Ok(()),
    }
}

pub(crate) fn time(call: &Call, name: &str) -> Result<Option<i64>, AdminError> {
    call.param(name)
        .map(|v| {
            UtcMicros::parse_rfc3339(v)
                .map(|t| t.0)
                .map_err(|_| AdminError::invalid_parameter(format!("{name} must be an RFC 3339 time")))
        })
        .transpose()
}

fn values<'a>(call: &'a Call, name: &str) -> Vec<&'a str> {
    call.query
        .iter()
        .filter(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .collect()
}

fn number(call: &Call, name: &str) -> Result<Option<f64>, AdminError> {
    call.param(name)
        .map(|v| {
            v.parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .ok_or_else(|| AdminError::invalid_parameter(format!("{name} must be a non-negative number")))
        })
        .transpose()
}

fn money(call: &Call, name: &str) -> Result<Option<i64>, AdminError> {
    call.param(name)
        .map(|v| {
            Nusd::parse_usd(v)
                .ok()
                .and_then(|n| i64::try_from(n.0).ok())
                .filter(|n| *n >= 0)
                .ok_or_else(|| AdminError::invalid_parameter(format!("{name} must be a decimal USD amount")))
        })
        .transpose()
}

/// The scopes whose records `permission` lets the principal read; `None` for every record.
pub(crate) fn scopes(principal: &AdminPrincipal, permission: &str) -> Option<Vec<ScopeRef>> {
    if principal.allows(permission, &[Scope::Gateway]) {
        return None;
    }
    Some(
        principal
            .bindings
            .iter()
            .filter(|b| b.role.allows(permission))
            .filter_map(|b| match &b.scope {
                Scope::Gateway => None,
                Scope::Organization(n) => Some(ScopeRef::Organization(n.clone())),
                Scope::Team(n) => Some(ScopeRef::Team(n.clone())),
                Scope::Project(n) => Some(ScopeRef::Project(n.clone())),
            })
            .collect(),
    )
}

/// Parses the selection of `call` for a caller holding `permission`; `content` lets `q` search
/// stored prompts.
pub(crate) fn parse(call: &Call, permission: &str, content: bool) -> Result<Selection, AdminError> {
    let mut f = LogFilter {
        from_us: time(call, "from")?,
        to_us: time(call, "to")?,
        q: call.param("q").map(str::to_owned).filter(|q| !q.is_empty()),
        q_content: content,
        scopes: scopes(&call.principal, permission),
        ..LogFilter::default()
    };
    let mut rollup = RollupFilter {
        scopes: f.scopes.clone(),
        ..RollupFilter::default()
    };
    let mut rollup_ok = f.q.is_none();
    for (param, column, rollup_column) in EQUALS {
        let vs = values(call, param);
        if vs.is_empty() {
            continue;
        }
        let args = match column {
            LogColumn::StatusCode => vs
                .iter()
                .map(|v| {
                    v.parse::<i64>()
                        .map(Arg::Int)
                        .map_err(|_| AdminError::invalid_parameter("status_code must be an integer"))
                })
                .collect::<Result<Vec<_>, _>>()?,
            LogColumn::Cache => {
                if let Some(bad) = vs.iter().find(|v| !CACHE_VALUES.contains(v)) {
                    return Err(AdminError::invalid_parameter(format!(
                        "cache must be one of hit, miss, semantic_hit, bypass, not {bad:?}"
                    )));
                }
                vs.iter().map(|v| Arg::Text((*v).to_owned())).collect()
            }
            _ => vs.iter().map(|v| Arg::Text((*v).to_owned())).collect(),
        };
        f.any_of.push((*column, args));
        match rollup_column.and_then(|c| ROLLUP_DIMS.iter().position(|d| *d == c)) {
            Some(i) => rollup
                .any_of
                .push((i, vs.iter().map(|v| (*v).to_owned()).collect())),
            None => rollup_ok = false,
        }
    }
    for v in values(call, "status_class") {
        let class = v
            .strip_suffix("xx")
            .and_then(|d| d.parse::<u8>().ok())
            .filter(|d| (1..=5).contains(d))
            .ok_or_else(|| AdminError::invalid_parameter("status_class must be one of 1xx to 5xx"))?;
        f.status_classes.push(class);
    }
    rollup.status_classes = f.status_classes.clone();
    f.tags = values(call, "tag").into_iter().map(str::to_owned).collect();
    f.guardrails = values(call, "guardrail").into_iter().map(str::to_owned).collect();
    f.verdicts = values(call, "guardrail_verdict")
        .into_iter()
        .map(str::to_owned)
        .collect();
    f.categories = values(call, "guardrail_category")
        .into_iter()
        .map(str::to_owned)
        .collect();
    f.flagged = bool_param(call, "flagged")?;
    f.stream = bool_param(call, "stream")?;
    if let Some(s) = f.stream
        && let Some(i) = ROLLUP_DIMS.iter().position(|d| *d == "stream")
    {
        rollup.any_of.push((i, vec![s.to_string()]));
    }
    f.latency_min = number(call, "latency_ms_min")?;
    f.latency_max = number(call, "latency_ms_max")?;
    f.cost_min_nusd = money(call, "cost_usd_min")?;
    f.cost_max_nusd = money(call, "cost_usd_max")?;
    if let (Some(from), Some(to)) = (f.from_us, f.to_us)
        && from > to
    {
        return Err(AdminError::invalid_parameter("from must not be after to"));
    }
    rollup_ok &= f.tags.is_empty()
        && f.guardrails.is_empty()
        && f.verdicts.is_empty()
        && f.categories.is_empty()
        && f.flagged.is_none()
        && f.latency_min.is_none()
        && f.latency_max.is_none()
        && f.cost_min_nusd.is_none()
        && f.cost_max_nusd.is_none();
    let metadata: Vec<(String, String)> = call
        .query
        .iter()
        .filter(|(k, _)| k.starts_with("metadata."))
        .cloned()
        .collect();
    Ok(Selection {
        filter: f,
        rollup: (rollup_ok && metadata.is_empty()).then_some(rollup),
        metadata,
    })
}

fn bool_param(call: &Call, name: &str) -> Result<Option<bool>, AdminError> {
    match call.param(name) {
        None => Ok(None),
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(AdminError::invalid_parameter(format!(
            "{name} must be true or false"
        ))),
    }
}

/// The group-by dimensions: comma-separated or repeated, at most three.
pub(crate) fn group_by(call: &Call) -> Result<Vec<Dim>, AdminError> {
    let mut out = Vec::new();
    for v in values(call, "group_by") {
        for name in v.split(',').filter(|s| !s.is_empty()) {
            let d = Dim::parse(name)
                .ok_or_else(|| AdminError::invalid_parameter(format!("group_by does not accept {name:?}")))?;
            if !out.contains(&d) {
                out.push(d);
            }
        }
    }
    if out.len() > 3 {
        return Err(AdminError::invalid_parameter(
            "group_by accepts at most 3 dimensions",
        ));
    }
    Ok(out)
}

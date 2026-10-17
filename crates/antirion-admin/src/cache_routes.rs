//! The operations of the Cache screen that are not configuration resources: the entry browser and
//! false-positive reports (`cache_entries`), and invalidation rules run by hand and their firings.
//! The rules themselves are the nested collection `cache/invalidation_rules`.

use antirion_core::UtcMicros;
use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::Handled;
use crate::call::Call;
use crate::error::AdminError;
use crate::pagination;

const PERMISSION: &str = "cache:admin";
const DAY_US: i64 = 86_400_000_000;

/// Whether `segs` is one of these operations.
pub(crate) fn routes(segs: &[&str]) -> bool {
    match segs {
        ["cache", "entries"] | ["cache", "entries", _] | ["runtime", "cache", "invalidation_rules"] => true,
        ["cache", "invalidation_rules", rule] => rule.ends_with(":run"),
        _ => false,
    }
}

pub(crate) fn dispatch<'a>(call: &'a Call, segs: &'a [&'a str]) -> Handled<'a> {
    match segs {
        ["cache", "entries"] => Box::pin(crate::cache_entries::list(call)),
        ["cache", "entries", action] if action.ends_with(":report_false_positive") => {
            let key = action.trim_end_matches(":report_false_positive");
            Box::pin(crate::cache_entries::report_false_positive(call, key))
        }
        ["cache", "entries", key] if call.method == Method::GET => {
            Box::pin(crate::cache_entries::get(call, key))
        }
        ["cache", "entries", key] => Box::pin(crate::cache::delete_entry(call, key)),
        ["cache", "invalidation_rules", action] => Box::pin(run(call, action.trim_end_matches(":run"))),
        ["runtime", "cache", "invalidation_rules"] => Box::pin(firings(call)),
        _ => crate::routes::done(Err(AdminError::not_found())),
    }
}

/// `POST /cache/invalidation_rules/{rule}:run`.
async fn run(call: &Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    call.require(PERMISSION, &[Scope::Gateway])?;
    match call.rt.run_cache_rule(name).await {
        None => Err(AdminError::not_found()),
        Some(Err(e)) => Err(AdminError::conflict("conflict", e.to_string(), Vec::new())),
        Some(Ok(dropped)) => {
            call.audit(
                "cache.run_rule",
                &format!("cache/invalidation_rules/{name}"),
                Outcome::Success,
                StatusCode::OK,
            )
            .await;
            Ok(call.json(StatusCode::OK, &json!({"dropped": dropped})))
        }
    }
}

/// `GET /runtime/cache/invalidation_rules`: every rule of the configuration, by name, with its
/// firings in the last 24 hours, every node's.
async fn firings(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["limit", "cursor"])?;
    call.require(PERMISSION, &[Scope::Gateway])?;
    let (limit, after) = pagination::page_params(call, "runtime/cache/invalidation_rules")?;
    let now = call.rt.now().0;
    let fired = call
        .rt
        .store
        .rule_firings(now - DAY_US)
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let mut names: Vec<&String> = call
        .snap
        .config
        .doc
        .pointer("/cache/invalidation_rules")
        .and_then(Value::as_object)
        .map(|m| m.keys().collect())
        .unwrap_or_default();
    names.sort();
    let mut rest = names
        .into_iter()
        .filter(|n| after.as_deref().is_none_or(|a| n.as_str() > a))
        .peekable();
    let mut items = Vec::new();
    while items.len() < limit {
        let Some(name) = rest.next() else {
            break;
        };
        let f = fired.iter().find(|f| &f.rule == name);
        items.push(json!({
            "name": name,
            "fires_24h": f.map_or(0, |f| f.recent),
            "last_fired_at": f.and_then(|f| f.last_fired_us).map(|t| UtcMicros(t).to_rfc3339_micros()),
            "last_dropped": f.map_or(0, |f| f.last_dropped),
        }));
    }
    let next = rest.peek().is_some().then(|| {
        let last = items
            .last()
            .and_then(|i| i.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        pagination::encode(&call.rt.cursor_key, "runtime/cache/invalidation_rules", last)
    });
    Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": next})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_these_operations_are_claimed() {
        assert!(routes(&["cache", "entries"]));
        assert!(routes(&["cache", "entries", "ab"]));
        assert!(routes(&["cache", "invalidation_rules", "nightly:run"]));
        assert!(routes(&["runtime", "cache", "invalidation_rules"]));
        assert!(!routes(&["cache", "invalidation_rules", "nightly"]));
        assert!(!routes(&["cache", "invalidation_rules"]));
        assert!(!routes(&["runtime", "cache"]));
    }
}

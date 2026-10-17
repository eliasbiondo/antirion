//! The operations of the danger zone and the encryption settings: ingress pauses, organization
//! export and scheduled deletion, export jobs, rotation of gateway-generated secrets and the
//! encryption report; and the body checks they share.

use antirion_config::model::{self, DEFAULT_TENANT};
use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_identity::rbac::Scope;
use serde_json::{Map, Value};

use crate::Handled;
use crate::call::Call;
use crate::error::AdminError;

/// Whether `segs` is one of these operations.
pub(crate) fn routes(segs: &[&str]) -> bool {
    match segs {
        [
            "runtime",
            "ingress" | "ingress:pause" | "ingress:resume" | "encryption",
        ]
        | ["jobs", _]
        | ["jobs", _, "download"]
        | ["settings", "security:rotate_secrets"] => true,
        ["organizations", action] => action_of(action).is_some(),
        _ => false,
    }
}

/// The organization and action of `<name>:export`, `:schedule_deletion` or `:cancel_deletion`.
fn action_of(segment: &str) -> Option<(&str, &str)> {
    segment
        .rsplit_once(':')
        .filter(|(_, a)| matches!(*a, "export" | "schedule_deletion" | "cancel_deletion"))
}

pub(crate) fn dispatch<'a>(call: &'a Call, segs: &'a [&'a str]) -> Handled<'a> {
    match segs {
        ["runtime", "ingress"] => Box::pin(crate::ingress::state(call)),
        ["runtime", "ingress:pause"] => Box::pin(crate::ingress::pause(call)),
        ["runtime", "ingress:resume"] => Box::pin(crate::ingress::resume(call)),
        ["runtime", "encryption"] => Box::pin(crate::encryption::report(call)),
        ["jobs", id] => Box::pin(crate::jobs::get(call, id)),
        ["jobs", id, "download"] => Box::pin(crate::jobs::download(call, id)),
        ["settings", "security:rotate_secrets"] => Box::pin(crate::rotate::rotate(call)),
        ["organizations", segment] => match action_of(segment) {
            Some((name, "export")) => Box::pin(crate::jobs::export(call, name)),
            Some((name, "schedule_deletion")) => Box::pin(crate::org_deletion::schedule(call, name)),
            Some((name, _)) => Box::pin(crate::org_deletion::cancel(call, name)),
            None => crate::routes::done(Err(AdminError::not_found())),
        },
        _ => crate::routes::done(Err(AdminError::not_found())),
    }
}

/// The permission an approver of a held organization action needs, on the organization: the
/// organization's own pause, or a scheduled deletion. `None` for every other action.
pub(crate) fn needed(action: &str, request: &Value) -> Option<Vec<(String, Vec<Scope>)>> {
    let change = request.get("changes")?.get(0)?;
    let (permission, org) = match action {
        "ingress_pause" => (
            "organizations:update",
            change.pointer("/body/organization")?.as_str()?.to_owned(),
        ),
        "organization_delete" => {
            let path = change.get("path")?.as_str()?;
            let segment = path.split('?').next()?.rsplit('/').next()?;
            let (name, _) = action_of(segment)?;
            ("organizations:delete", crate::percent_decode(name))
        }
        _ => return None,
    };
    Some(vec![(
        permission.to_owned(),
        vec![Scope::Gateway, Scope::Organization(org)],
    )])
}

/// Refuses any method but `method`.
pub(crate) fn only(call: &Call, method: http::Method, allow: &'static str) -> Result<(), AdminError> {
    if call.method == method {
        Ok(())
    } else {
        Err(AdminError::method_not_allowed(allow))
    }
}

/// A validation error at a body member.
pub(crate) fn body_error(member: &str, code: DetailCode, message: &str) -> AdminError {
    AdminError::validation(vec![ConfigError::new(
        &DocPath::root().key(member),
        code,
        message,
    )])
}

/// The JSON object body of an action (an empty body is `{}`), with no member outside `allowed`.
pub(crate) fn body(call: &Call, allowed: &[&str]) -> Result<Map<String, Value>, AdminError> {
    let m = if call.body.iter().all(u8::is_ascii_whitespace) {
        Map::new()
    } else {
        call.json_body()?
    };
    if let Some(k) = m.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(body_error(k, DetailCode::UnknownField, "unknown member"));
    }
    Ok(m)
}

/// An optional string member.
pub(crate) fn text<'a>(m: &'a Map<String, Value>, member: &str) -> Result<Option<&'a str>, AdminError> {
    match m.get(member) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(body_error(member, DetailCode::TypeMismatch, "must be a string")),
    }
}

/// An optional duration member within `min ..= max`.
pub(crate) fn duration(
    m: &Map<String, Value>,
    member: &str,
    min: std::time::Duration,
    max: std::time::Duration,
) -> Result<Option<std::time::Duration>, AdminError> {
    let Some(s) = text(m, member)? else {
        return Ok(None);
    };
    let d = antirion_config::scalar::parse_duration(s)
        .ok_or_else(|| body_error(member, DetailCode::InvalidFormat, "must be a duration"))?;
    if d < min || d > max {
        return Err(body_error(
            member,
            DetailCode::OutOfRange,
            &format!(
                "must be from {} to {}",
                antirion_config::scalar::format_duration(min),
                antirion_config::scalar::format_duration(max)
            ),
        ));
    }
    Ok(Some(d))
}

/// The scope chain of organization `name`, when it exists and the caller may see it; 404
/// otherwise.
pub(crate) fn organization(call: &Call, name: &str) -> Result<Vec<Scope>, AdminError> {
    let exists = name == DEFAULT_TENANT
        || model::collection("organizations")
            .and_then(|c| call.snap.config.resource(c, name))
            .is_some();
    let chain = vec![Scope::Gateway, Scope::Organization(name.to_owned())];
    if !exists || !call.principal.sees(&chain) {
        return Err(AdminError::not_found());
    }
    Ok(chain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_these_operations_are_claimed() {
        assert!(routes(&["runtime", "ingress"]) && routes(&["runtime", "ingress:pause"]));
        assert!(routes(&["runtime", "encryption"]));
        assert!(routes(&["jobs", "j1"]) && routes(&["jobs", "j1", "download"]));
        assert!(routes(&["organizations", "acme:export"]));
        assert!(routes(&["organizations", "acme:schedule_deletion"]));
        assert!(routes(&["organizations", "acme:cancel_deletion"]));
        assert!(routes(&["settings", "security:rotate_secrets"]));
        assert!(!routes(&["organizations", "acme"]) && !routes(&["organizations", "acme:rename"]));
        assert!(!routes(&["runtime", "budgets"]) && !routes(&["settings", "security"]));
        assert_eq!(action_of("a:b:export"), Some(("a:b", "export")));
    }
}

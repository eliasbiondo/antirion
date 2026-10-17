//! Rules of the console's identity settings the schema cannot express: bounds that relate two
//! fields, password sign-in refusals that would lock users out, plain SMTP to a remote host, and the
//! display references of teams and users.

use serde_json::Value;

use crate::scalar::parse_duration;
use crate::{ConfigError, DetailCode, DocPath};

fn number(doc: &Value, pointer: &str) -> Option<u64> {
    doc.pointer(pointer).and_then(Value::as_u64)
}

fn duration(doc: &Value, pointer: &str) -> Option<std::time::Duration> {
    doc.pointer(pointer)
        .and_then(Value::as_str)
        .and_then(parse_duration)
}

fn has_sso(doc: &Value) -> bool {
    [
        "/auth/admin/sso/oidc",
        "/auth/admin/sso/oauth",
        "/auth/admin/sso/saml",
    ]
    .iter()
    .any(|p| {
        doc.pointer(p)
            .and_then(Value::as_object)
            .is_some_and(|m| !m.is_empty())
    })
}

/// Whether a host name or address is this machine.
fn loopback(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare.eq_ignore_ascii_case("localhost")
        || bare.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn exists(doc: &Value, collection: &str, name: &str) -> bool {
    name == crate::model::DEFAULT_TENANT && collection == "teams"
        || doc.get(collection).and_then(|c| c.get(name)).is_some()
}

/// The largest image a `data:` logo may carry.
const MAX_LOGO_BYTES: usize = 64 * 1024;

/// Why an organization's `logo` is refused: a `data:` URI must be valid base64 of at most 64 KiB.
fn logo_problem(logo: &str) -> Option<&'static str> {
    let (_, data) = logo.split_once(";base64,")?;
    if !logo.starts_with("data:") {
        return None;
    }
    let body = data.trim_end_matches('=');
    if data.len() % 4 != 0
        || data.len() - body.len() > 2
        || !body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
    {
        return Some("the data URI is not valid base64");
    }
    (body.len() * 3 / 4 > MAX_LOGO_BYTES).then_some("the image is larger than 64 KiB")
}

pub(crate) fn check(doc: &Value, errors: &mut Vec<ConfigError>) {
    let root = DocPath::root();
    let local = root.key("auth").key("admin").key("local");
    let min = number(doc, "/auth/admin/local/password_policy/min_length").unwrap_or(12);
    if let Some(max) = number(doc, "/auth/admin/local/password_policy/max_length")
        && max < min
    {
        errors.push(ConfigError::new(
            &local.key("password_policy").key("max_length"),
            DetailCode::OutOfRange,
            "max_length must be at least min_length",
        ));
    }
    let trusted = "/auth/admin/session/trusted_device";
    let max_age =
        duration(doc, &format!("{trusted}/max_age")).unwrap_or(std::time::Duration::from_secs(30 * 86_400));
    if let Some(idle) = duration(doc, &format!("{trusted}/idle_timeout"))
        && idle > max_age
    {
        errors.push(ConfigError::new(
            &root
                .key("auth")
                .key("admin")
                .key("session")
                .key("trusted_device")
                .key("idle_timeout"),
            DetailCode::OutOfRange,
            "idle_timeout must not exceed max_age",
        ));
    }
    let sso = has_sso(doc);
    let domains = doc
        .pointer("/auth/admin/local/sso_required_domains")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty());
    if domains && !sso {
        errors.push(ConfigError::new(
            &local.key("sso_required_domains"),
            DetailCode::Conflict,
            "requiring single sign-on needs at least one single sign-on provider",
        ));
    }
    for (name, org) in doc
        .get("organizations")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if org.get("local_login") == Some(&Value::Bool(false)) && !sso {
            errors.push(ConfigError::new(
                &root.key("organizations").key(name).key("local_login"),
                DetailCode::Conflict,
                "refusing password sign-in needs at least one single sign-on provider",
            ));
        }
    }
    if doc.pointer("/smtp/tls").and_then(Value::as_str) == Some("none")
        && let Some(host) = doc.pointer("/smtp/host").and_then(Value::as_str)
        && !loopback(host)
    {
        errors.push(ConfigError::new(
            &root.key("smtp").key("tls"),
            DetailCode::Conflict,
            "tls none is accepted only for a loopback host",
        ));
    }
    for (name, org) in doc
        .get("organizations")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let at = root.key("organizations").key(name);
        if let Some(tz) = org.get("timezone").and_then(Value::as_str)
            && !antirion_core::time::is_time_zone(tz)
        {
            errors.push(ConfigError::new(
                &at.key("timezone"),
                DetailCode::InvalidFormat,
                format!("{tz:?} is not an IANA time zone"),
            ));
        }
        if let Some(logo) = org.get("logo").and_then(Value::as_str)
            && let Some(message) = logo_problem(logo)
        {
            errors.push(ConfigError::new(
                &at.key("logo"),
                DetailCode::InvalidFormat,
                message,
            ));
        }
    }
    for (name, team) in doc.get("teams").and_then(Value::as_object).into_iter().flatten() {
        if let Some(lead) = team.get("lead").and_then(Value::as_str)
            && !exists(doc, "users", lead)
        {
            errors.push(ConfigError::new(
                &root.key("teams").key(name).key("lead"),
                DetailCode::UnresolvedReference,
                format!("user {lead:?} does not exist"),
            ));
        }
    }
    for (name, user) in doc.get("users").and_then(Value::as_object).into_iter().flatten() {
        if let Some(team) = user.get("primary_team").and_then(Value::as_str)
            && !exists(doc, "teams", team)
        {
            errors.push(ConfigError::new(
                &root.key("users").key(name).key("primary_team"),
                DetailCode::UnresolvedReference,
                format!("team {team:?} does not exist"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn found(doc: &Value) -> Vec<(String, DetailCode)> {
        let mut errors = Vec::new();
        check(doc, &mut errors);
        errors.into_iter().map(|e| (e.path, e.code)).collect()
    }

    #[test]
    fn related_bounds_and_lockouts() {
        let doc = json!({
            "auth": {"admin": {
                "local": {"password_policy": {"min_length": 20, "max_length": 16}, "sso_required_domains": ["acme.example"]},
                "session": {"trusted_device": {"max_age": "2d", "idle_timeout": "3d"}}
            }},
            "organizations": {
                "acme": {"local_login": false},
                "o": {"timezone": "Mars/Olympus", "logo": format!("data:image/png;base64,{}", "A".repeat(90_000))}
            },
            "smtp": {"host": "mail.acme.example", "tls": "none"},
            "teams": {"t": {"lead": "ghost"}},
            "users": {"u": {"primary_team": "nope"}}
        });
        let got = found(&doc);
        for want in [
            (
                "auth.admin.local.password_policy.max_length",
                DetailCode::OutOfRange,
            ),
            (
                "auth.admin.session.trusted_device.idle_timeout",
                DetailCode::OutOfRange,
            ),
            ("auth.admin.local.sso_required_domains", DetailCode::Conflict),
            ("organizations.acme.local_login", DetailCode::Conflict),
            ("smtp.tls", DetailCode::Conflict),
            ("teams.t.lead", DetailCode::UnresolvedReference),
            ("organizations.o.timezone", DetailCode::InvalidFormat),
            ("organizations.o.logo", DetailCode::InvalidFormat),
            ("users.u.primary_team", DetailCode::UnresolvedReference),
        ] {
            assert!(
                got.contains(&(want.0.to_owned(), want.1)),
                "{want:?} not in {got:?}"
            );
        }
    }

    #[test]
    fn valid_settings_pass() {
        let doc = json!({
            "auth": {"admin": {"sso": {"oidc": {"corp": {"issuer": "https://i"}}},
                               "local": {"sso_required_domains": ["acme.example"]}}},
            "organizations": {"acme": {"local_login": false, "timezone": "America/Sao_Paulo",
                                       "logo": "data:image/png;base64,iVBORw0KGgo="}},
            "smtp": {"host": "127.0.0.1", "tls": "none"},
            "teams": {"t": {"lead": "u"}},
            "users": {"u": {"primary_team": "default"}}
        });
        assert!(found(&doc).is_empty(), "{:?}", found(&doc));
        assert!(loopback("[::1]") && loopback("localhost") && !loopback("10.0.0.1"));
    }
}

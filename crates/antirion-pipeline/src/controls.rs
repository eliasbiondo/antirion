//! Request controls carried in headers (check 9 of `authenticate`): each one that widens what a key
//! can do, reveals internals or relaxes a setting needs its key permission, and a control is never
//! silently ignored.

use std::time::Duration;

use antirion_config::scalar::duration_nanos;
use antirion_core::Stage;
use antirion_error::reason::hygiene;
use antirion_error::{GatewayError, Message};
use antirion_identity::KeyPermission;
use antirion_identity::access::pattern_matches;
use antirion_identity::reasons;
use antirion_runtime::identity::KeyRt;
use http::HeaderMap;
use serde_json::Value;

use crate::ctx::Ctx;

pub(crate) const PIN_DEPLOYMENT: &str = "x-antirion-pin-deployment";
pub(crate) const ROUTE_DEBUG: &str = "x-antirion-route-debug";
pub(crate) const ATTEMPT_TRAIL: &str = "x-antirion-attempt-trail";
pub(crate) const END_USER: &str = "x-antirion-end-user";
const DEBUG: &str = "x-antirion-debug";
const FALLBACKS: &str = "x-antirion-fallbacks";
const PROVIDER_KEY: &str = "x-antirion-provider-key";
const TAGS: &str = "x-antirion-tags";
const MAX_ATTEMPTS: &str = "x-antirion-max-attempts";
const TIMEOUT: &str = "x-antirion-timeout";
const NO_FALLBACK: &str = "x-antirion-no-fallback";
const METADATA: &str = "x-antirion-metadata";
const SESSION: &str = "x-antirion-session-id";
pub(crate) const AFFINITY: &str = "x-antirion-affinity";
const LOG_CONTENT: &str = "x-antirion-log-content";

const STAINLESS_READ_TIMEOUT: &str = "x-stainless-read-timeout";
const STAINLESS_TIMEOUT: &str = "x-stainless-timeout";
const MAX_FALLBACKS: usize = 5;
const DEFAULT_MAX_ATTEMPTS: u64 = 5;
const DEFAULT_DEADLINE: Duration = Duration::from_secs(900);
const MAX_TAGS: usize = 32;

/// The controls a request may use, after their permissions were checked.
#[derive(Clone, Debug, Default)]
pub(crate) struct Controls {
    pub(crate) pin: Option<String>,
    pub(crate) route_debug: bool,
    pub(crate) attempt_trail: bool,
    pub(crate) tags: Vec<String>,
    /// `x-antirion-max-attempts`.
    pub(crate) max_attempts: Option<u32>,
    /// The header asked for more attempts than the key's levels configure, with `override:relax`.
    pub(crate) max_attempts_relaxed: bool,
    /// `x-antirion-timeout`: the request's deadline.
    pub(crate) timeout: Option<Duration>,
    /// `x-antirion-no-fallback: true`.
    pub(crate) no_fallback: bool,
    /// `x-antirion-affinity: off`.
    pub(crate) affinity_off: bool,
    /// The trimmed `x-antirion-session-id`.
    pub(crate) session: Option<String>,
    /// The parsed `x-antirion-metadata` object.
    pub(crate) metadata: Option<Value>,
    /// `x-antirion-log-content`, after its checks.
    pub(crate) log_content: Option<bool>,
    /// `x-antirion-fallbacks`: Model names whose tiers follow the route's.
    pub(crate) fallbacks: Vec<String>,
}

impl Controls {
    /// The attempt budget of a request whose policy configures `configured`: the header's value
    /// when it lowers it, or when a key with `override:relax` raised it.
    pub(crate) fn attempts(&self, configured: u32) -> u32 {
        match self.max_attempts {
            Some(n) if self.max_attempts_relaxed => n,
            Some(n) => n.min(configured),
            None => configured,
        }
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<Result<&'a str, ()>> {
    headers
        .get(name)
        .map(|v| v.to_str().map(str::trim).map_err(|_| ()))
}

pub(crate) fn invalid_header(name: &'static str, message: &'static str) -> GatewayError {
    GatewayError::with_reason(
        hygiene::INVALID_HEADER,
        Stage::Authenticate,
        Message::Fixed(message),
    )
    .param(name)
}

fn permission_required(name: &'static str, permission: KeyPermission) -> GatewayError {
    GatewayError::with_reason(
        reasons::PERMISSION_REQUIRED,
        Stage::Authenticate,
        Message::built(format!("The {name} control needs the `{permission}` permission.")),
    )
    .param(name)
}

fn require(key: &KeyRt, name: &'static str, permission: KeyPermission) -> Result<(), GatewayError> {
    if key.permissions.has(permission) {
        Ok(())
    } else {
        Err(permission_required(name, permission))
    }
}

/// The `policies` blocks that apply before the model is known: gateway, organization, team,
/// project and key, broadest first.
pub(crate) fn policy_levels<'a>(doc: &'a Value, key: &KeyRt) -> Vec<&'a Value> {
    let tenant = |collection: &str, name: &str| doc.get(collection)?.get(name)?.get("policies");
    [
        doc.get("policies"),
        tenant("organizations", &key.organization),
        tenant("teams", &key.team),
        tenant("projects", &key.project),
        tenant("keys", &key.name),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn narrowest<'a>(levels: &[&'a Value], pointer: &str) -> Option<&'a Value> {
    levels.iter().rev().find_map(|l| l.pointer(pointer))
}

/// A request override of a resilience field: refused when the field is locked anywhere or not
/// request-overridable at the narrowest level that says.
fn overridable(levels: &[&Value], field: &str, name: &'static str) -> Result<(), GatewayError> {
    let locked = levels.iter().any(|l| {
        l.pointer("/resilience/locked")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|f| f.as_str() == Some(field)))
    });
    let listed = narrowest(levels, "/resilience/request_overridable")
        .and_then(Value::as_array)
        .is_none_or(|a| a.iter().any(|f| f.as_str() == Some(field)));
    if locked || !listed {
        return Err(GatewayError::with_reason(
            reasons::OVERRIDE_NOT_ALLOWED,
            Stage::Authenticate,
            Message::built(format!(
                "The setting `{field}` cannot be overridden by a request."
            )),
        )
        .param(name));
    }
    Ok(())
}

fn relax(key: &KeyRt, name: &'static str, relaxing: bool) -> Result<(), GatewayError> {
    if relaxing {
        require(key, name, KeyPermission::OverrideRelax)
    } else {
        Ok(())
    }
}

fn tag_ok(tag: &str) -> bool {
    (1..=64).contains(&tag.len())
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'.' | b'-'))
}

fn tag_name(item: &str) -> &str {
    item.strip_prefix(['!', '&']).unwrap_or(item)
}

/// The items of an `x-antirion-tags` value, each a tag name with an optional `!` or `&` marker;
/// `None` when the value is malformed.
pub(crate) fn tag_items(value: &str) -> Option<Vec<&str>> {
    let items: Vec<&str> = value.split(',').map(str::trim).collect();
    (items.len() <= MAX_TAGS && items.iter().all(|t| tag_ok(tag_name(t)))).then_some(items)
}

fn tags(key: &KeyRt, value: &str) -> Result<Vec<String>, GatewayError> {
    let Some(items) = tag_items(value) else {
        return Err(invalid_header(TAGS, "The x-antirion-tags header is malformed."));
    };
    let names: Vec<&str> = items.iter().map(|t| tag_name(t)).collect();
    if names
        .iter()
        .any(|t| t.get(..9).is_some_and(|p| p.eq_ignore_ascii_case("antirion.")))
    {
        return Err(GatewayError::with_reason(
            reasons::RESERVED_TAG,
            Stage::Authenticate,
            Message::Fixed("Tags in the antirion. namespace are reserved for the gateway."),
        )
        .param(TAGS));
    }
    require(key, TAGS, KeyPermission::TagsRequest)?;
    if let Some(t) = names
        .iter()
        .find(|t| !key.allowed_request_tags.iter().any(|p| pattern_matches(p, t)))
    {
        return Err(GatewayError::with_reason(
            reasons::PERMISSION_REQUIRED,
            Stage::Authenticate,
            Message::built(format!("The key may not attach the request tag `{t}`.")),
        )
        .param(TAGS));
    }
    Ok(items.iter().map(|t| (*t).to_owned()).collect())
}

/// Checks every header control of the request against the key's permissions.
pub(crate) fn check(cx: &Ctx, headers: &HeaderMap, key: &KeyRt) -> Result<Controls, GatewayError> {
    let mut controls = Controls::default();
    let doc = &cx.snap.config.doc;
    let levels = policy_levels(doc, key);

    if let Some(v) = header(headers, ATTEMPT_TRAIL) {
        if v != Ok("true") {
            return Err(invalid_header(
                ATTEMPT_TRAIL,
                "x-antirion-attempt-trail must be true.",
            ));
        }
        match doc
            .pointer("/resilience/exposure/attempt_trail")
            .and_then(Value::as_str)
            .unwrap_or("on_request")
        {
            "off" => {}
            "always" => controls.attempt_trail = true,
            _ => {
                require(key, ATTEMPT_TRAIL, KeyPermission::Debug)?;
                controls.attempt_trail = true;
            }
        }
    }
    if doc
        .pointer("/resilience/exposure/attempt_trail")
        .and_then(Value::as_str)
        == Some("always")
    {
        controls.attempt_trail = true;
    }
    if let Some(v) = header(headers, DEBUG) {
        if v != Ok("true") {
            return Err(invalid_header(DEBUG, "x-antirion-debug must be true."));
        }
        require(key, DEBUG, KeyPermission::Debug)?;
        if doc
            .pointer("/resilience/exposure/attempt_trail")
            .and_then(Value::as_str)
            != Some("off")
        {
            controls.attempt_trail = true;
        }
    }
    if let Some(v) = header(headers, ROUTE_DEBUG) {
        if v != Ok("1") && v != Ok("true") {
            return Err(invalid_header(ROUTE_DEBUG, "x-antirion-route-debug must be 1."));
        }
        require(key, ROUTE_DEBUG, KeyPermission::RouteDebug)?;
        controls.route_debug = true;
    }
    if let Some(v) = header(headers, PIN_DEPLOYMENT) {
        require(key, PIN_DEPLOYMENT, KeyPermission::RoutePin)?;
        match v {
            Ok(name) if !name.is_empty() => controls.pin = Some(name.to_owned()),
            _ => {
                return Err(invalid_header(
                    PIN_DEPLOYMENT,
                    "x-antirion-pin-deployment must name a deployment.",
                ));
            }
        }
    }
    if let Some(v) = header(headers, FALLBACKS) {
        let malformed = || invalid_header(FALLBACKS, "x-antirion-fallbacks must list 1 to 5 Model names.");
        let names: Vec<String> = v
            .map_err(|()| malformed())?
            .split(',')
            .map(|n| n.trim().to_owned())
            .collect();
        if names.len() > MAX_FALLBACKS || names.iter().any(String::is_empty) {
            return Err(malformed());
        }
        require(key, FALLBACKS, KeyPermission::RouteFallbacks)?;
        controls.fallbacks = names;
    }
    if headers.contains_key(PROVIDER_KEY) {
        require(key, PROVIDER_KEY, KeyPermission::Byok)?;
    }
    if let Some(v) = header(headers, TAGS) {
        let value = v.map_err(|()| invalid_header(TAGS, "The x-antirion-tags header is malformed."))?;
        controls.tags = tags(key, value)?;
    }
    if let Some(v) = header(headers, MAX_ATTEMPTS) {
        let n = v
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| (1..=10).contains(n))
            .ok_or_else(|| {
                invalid_header(
                    MAX_ATTEMPTS,
                    "x-antirion-max-attempts must be an integer from 1 to 10.",
                )
            })?;
        overridable(&levels, "retry.max_attempts", MAX_ATTEMPTS)?;
        let configured = narrowest(&levels, "/resilience/retry/max_attempts")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_MAX_ATTEMPTS);
        relax(key, MAX_ATTEMPTS, n > configured)?;
        controls.max_attempts = u32::try_from(n).ok();
        controls.max_attempts_relaxed = n > configured;
    }
    if let Some(v) = header(headers, TIMEOUT) {
        let asked = v
            .ok()
            .and_then(duration_nanos)
            .filter(|n| (1_000_000..=86_400_000_000_000).contains(n))
            .ok_or_else(|| {
                invalid_header(TIMEOUT, "x-antirion-timeout must be a duration from 1ms to 24h.")
            })?;
        overridable(&levels, "timeouts.deadline", TIMEOUT)?;
        let configured = match narrowest(&levels, "/resilience/timeouts/deadline").and_then(Value::as_str) {
            Some("none") => None,
            Some(d) => duration_nanos(d),
            None => Some(DEFAULT_DEADLINE.as_nanos()),
        };
        relax(key, TIMEOUT, configured.is_some_and(|c| asked > c))?;
        controls.timeout = u64::try_from(asked).ok().map(Duration::from_nanos);
    }
    if let Some(v) = header(headers, NO_FALLBACK) {
        let v = v.map(str::to_ascii_lowercase);
        let v = v.as_deref().map_err(|_| ());
        if v != Ok("true") && v != Ok("false") {
            return Err(invalid_header(
                NO_FALLBACK,
                "x-antirion-no-fallback must be true or false.",
            ));
        }
        overridable(&levels, "retry.fallback", NO_FALLBACK)?;
        controls.no_fallback = v == Ok("true");
    }
    if let Some(v) = header(headers, LOG_CONTENT) {
        controls.log_content = Some(log_content(key, &levels, v)?);
    }
    routing_headers(cx, headers, &mut controls)?;
    Ok(controls)
}

/// The read timeout the client's SDK announces (`x-stainless-read-timeout`, else
/// `x-stainless-timeout`, in decimal seconds); a value that is not a positive number is ignored.
pub(crate) fn read_timeout(headers: &HeaderMap) -> Option<Duration> {
    [STAINLESS_READ_TIMEOUT, STAINLESS_TIMEOUT]
        .iter()
        .find_map(|name| {
            headers
                .get(*name)?
                .to_str()
                .ok()?
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|secs| secs.is_finite() && *secs > 0.0)
                .and_then(|secs| Duration::try_from_secs_f64(secs).ok())
        })
}

/// The routing headers every key may send: session id, affinity opt-out and metadata, each
/// parsed strictly.
fn routing_headers(cx: &Ctx, headers: &HeaderMap, controls: &mut Controls) -> Result<(), GatewayError> {
    if let Some(v) = header(headers, SESSION) {
        let value = v
            .ok()
            .filter(|s| (1..=256).contains(&s.len()) && s.bytes().all(|b| (0x21..=0x7e).contains(&b)));
        let Some(value) = value else {
            return Err(invalid_header(
                SESSION,
                "x-antirion-session-id must be 1 to 256 visible ASCII characters.",
            ));
        };
        controls.session = Some(value.to_owned());
    }
    if let Some(v) = header(headers, AFFINITY) {
        if v != Ok("off") {
            return Err(invalid_header(AFFINITY, "x-antirion-affinity accepts only off."));
        }
        controls.affinity_off = true;
    }
    if let Some(raw) = headers.get(METADATA) {
        let max = cx.snap.routing.settings.metadata_max_size;
        let parsed = (raw.len() <= max)
            .then(|| serde_json::from_slice::<Value>(raw.as_bytes()).ok())
            .flatten()
            .filter(Value::is_object);
        let Some(meta) = parsed else {
            return Err(invalid_header(
                METADATA,
                "x-antirion-metadata must be a JSON object within the size limit.",
            ));
        };
        controls.metadata = Some(meta);
    }
    Ok(())
}

/// `x-antirion-log-content`: `false` always applies; `true` relaxes content logging, so it needs a
/// level to list `content.enabled` as request-overridable, no lock, no zero data retention, and
/// the key's `override:relax`.
fn log_content(key: &KeyRt, levels: &[&Value], v: Result<&str, ()>) -> Result<bool, GatewayError> {
    let value = match v {
        Ok(s) if s.eq_ignore_ascii_case("true") => true,
        Ok(s) if s.eq_ignore_ascii_case("false") => false,
        _ => {
            return Err(invalid_header(
                LOG_CONTENT,
                "x-antirion-log-content must be true or false.",
            ));
        }
    };
    if !value {
        return Ok(false);
    }
    let lists = |pointer: &str, l: &Value| {
        l.pointer(pointer)
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|f| f.as_str() == Some("content.enabled")))
    };
    let locked = levels.iter().any(|l| lists("/observability/locked", l));
    let listed = narrowest(levels, "/observability/request_overridable")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|f| f.as_str() == Some("content.enabled")));
    let zdr = levels
        .iter()
        .any(|l| l.pointer("/observability/zero_data_retention") == Some(&Value::Bool(true)));
    if locked || !listed || zdr {
        return Err(GatewayError::with_reason(
            reasons::OVERRIDE_NOT_ALLOWED,
            Stage::Authenticate,
            Message::Fixed("Content logging cannot be turned on by this request."),
        )
        .param(LOG_CONTENT));
    }
    require(key, LOG_CONTENT, KeyPermission::OverrideRelax)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn narrowest_level_wins_and_locks_apply_everywhere() {
        let gateway = json!({"resilience": {"retry": {"max_attempts": 3}, "locked": ["timeouts.deadline"]}});
        let key = json!({"resilience": {"retry": {"max_attempts": 2}}});
        let levels = [&gateway, &key];
        assert_eq!(
            narrowest(&levels, "/resilience/retry/max_attempts").and_then(Value::as_u64),
            Some(2)
        );
        assert!(overridable(&levels, "retry.max_attempts", MAX_ATTEMPTS).is_ok());
        let e = overridable(&levels, "timeouts.deadline", TIMEOUT).unwrap_err();
        assert_eq!(e.reason, Some(reasons::OVERRIDE_NOT_ALLOWED));
        let narrowed = json!({"resilience": {"request_overridable": ["retry.fallback"]}});
        assert!(overridable(&[&gateway, &narrowed], "retry.max_attempts", MAX_ATTEMPTS).is_err());
    }

    #[test]
    fn sdk_read_timeouts() {
        let mut h = HeaderMap::new();
        assert_eq!(read_timeout(&h), None);
        h.insert(STAINLESS_TIMEOUT, "2".parse().unwrap());
        assert_eq!(read_timeout(&h), Some(Duration::from_secs(2)));
        h.insert(STAINLESS_READ_TIMEOUT, "1.5".parse().unwrap());
        assert_eq!(read_timeout(&h), Some(Duration::from_millis(1500)));
        h.insert(STAINLESS_READ_TIMEOUT, "-1".parse().unwrap());
        assert_eq!(
            read_timeout(&h),
            Some(Duration::from_secs(2)),
            "an invalid value is ignored"
        );
        h.insert(STAINLESS_TIMEOUT, "soon".parse().unwrap());
        assert_eq!(read_timeout(&h), None);
    }

    #[test]
    fn tag_grammar() {
        assert!(tag_ok("exp:a"));
        assert!(!tag_ok("has space"));
        assert!(!tag_ok(""));
    }
}

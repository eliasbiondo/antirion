//! Key-specific rules of the Admin API: defaults and checks on create and update, a key's read-only
//! metadata, and rotation.

use std::time::Duration;

use antirion_config::model::{self, DEFAULT_TENANT};
use antirion_config::{ConfigError, DetailCode};
use antirion_core::UtcMicros;
use antirion_identity::{Credential, Plane};
use antirion_runtime::MetaEdit;
use antirion_runtime::keypolicy::KeyPolicy;
use antirion_runtime::state::{IMPORTED_PREFIX, KeyCredential, PreviousCredential, ResourceMeta};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::commit::{Proposal, api_write};
use crate::error::AdminError;
use crate::perms::{chain, owner};

const MAX_GRACE: Duration = Duration::from_secs(7 * 86_400);
/// The digest length of an imported key: SHA-256 in lowercase hex.
const DIGEST_LEN: usize = 64;
const GRANT: &str = "keys:grant_permissions";

fn permissions(spec: &Value) -> Vec<&str> {
    let mut p: Vec<&str> = spec
        .get("permissions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    p.sort_unstable();
    p.dedup();
    p
}

fn project_of(spec: &Value) -> &str {
    spec.get("project")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_TENANT)
}

/// A key's `expires_at` against the smallest `max_key_lifetime` above it: required, and no later than
/// `now` plus that lifetime.
fn check_lifetime(call: &Call, name: &str, spec: &Value) -> Result<(), AdminError> {
    let policy = KeyPolicy::of(&call.snap.config.doc, project_of(spec));
    let Some((max, level)) = policy.max_lifetime else {
        return Ok(());
    };
    let now = call.rt.now();
    let expires = spec
        .get("expires_at")
        .and_then(Value::as_str)
        .and_then(|s| UtcMicros::parse_rfc3339(s).ok());
    if expires.is_some_and(|t| t <= now.saturating_add(max)) {
        return Ok(());
    }
    let limit = antirion_config::scalar::format_duration(max);
    let path = model::collection("keys").map(|c| c.resource_path(name).key("expires_at"));
    Err(AdminError::validation(
        path.map(|p| {
            vec![ConfigError::new(
                &p,
                DetailCode::OutOfRange,
                format!("expires_at must be set, at most {limit} from now (max_key_lifetime of {level})"),
            )]
        })
        .unwrap_or_default(),
    ))
}

/// Fills a new key's defaults and checks what only a creation checks.
pub(crate) fn prepare_new(call: &Call, name: &str, spec: &mut Value) -> Result<(), AdminError> {
    let now = call.rt.now();
    let policy = KeyPolicy::of(&call.snap.config.doc, project_of(spec));
    if let Some(obj) = spec.as_object_mut() {
        obj.entry("project")
            .or_insert_with(|| Value::from(DEFAULT_TENANT));
        if !obj.contains_key("expires_at")
            && let Some(at) = policy.default_expiry(now)
        {
            obj.insert("expires_at".into(), Value::from(at.to_rfc3339_seconds()));
        }
    }
    if let Some(at) = spec.get("expires_at").and_then(Value::as_str)
        && let Ok(t) = UtcMicros::parse_rfc3339(at)
        && t <= call.rt.now()
    {
        let path = model::collection("keys").map(|c| c.resource_path(name).key("expires_at"));
        return Err(AdminError::validation(
            path.map(|p| {
                vec![ConfigError::new(
                    &p,
                    DetailCode::OutOfRange,
                    "expires_at is in the past",
                )]
            })
            .unwrap_or_default(),
        ));
    }
    if !permissions(spec).is_empty() {
        call.require(GRANT, &chain(&call.snap, "keys", name, Some(spec)))?;
    }
    check_lifetime(call, name, spec)
}

/// The credential of a key created from a legacy key's SHA-256 digest (`import`): it authenticates
/// the legacy secret itself until a rotation issues a normal one.
pub(crate) fn imported(call: &Call, spec: &Value, import: &Value) -> Result<KeyCredential, AdminError> {
    let at = antirion_config::DocPath::root().key("import");
    let invalid = |field: &str, code: DetailCode, message: &str| {
        AdminError::validation(vec![ConfigError::new(&at.key(field), code, message)])
    };
    if import.get("algorithm").and_then(Value::as_str) != Some("sha256") {
        return Err(invalid(
            "algorithm",
            DetailCode::InvalidEnum,
            "algorithm must be sha256",
        ));
    }
    let digest = import
        .get("digest")
        .and_then(Value::as_str)
        .filter(|d| {
            d.len() == DIGEST_LEN
                && d.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(|| {
            invalid(
                "digest",
                DetailCode::InvalidFormat,
                "digest must be 64 lowercase hex characters",
            )
        })?;
    if spec.get("secret").is_some() || spec.get("kind").and_then(Value::as_str) == Some("jwt") {
        return Err(invalid(
            "digest",
            DetailCode::Conflict,
            "an imported key has no declared secret and is not a JWT key",
        ));
    }
    if call.snap.identity.imported.contains_key(digest) {
        return Err(invalid(
            "digest",
            DetailCode::Conflict,
            "another key was imported with this digest",
        ));
    }
    Ok(KeyCredential {
        kid: String::new(),
        verifier: format!("{IMPORTED_PREFIX}{digest}"),
        hint: format!("imported-…{}", digest.get(DIGEST_LEN - 4..).unwrap_or_default()),
        previous: None,
    })
}

/// Changing a key's permissions needs `keys:grant_permissions` in its scope, and an updated key
/// must respect the maximum lifetime above it.
pub(crate) fn check_update(call: &Call, name: &str, old: &Value, new: &Value) -> Result<(), AdminError> {
    if permissions(old) != permissions(new) {
        call.require(GRANT, &chain(&call.snap, "keys", name, Some(old)))?;
    }
    check_lifetime(call, name, new)
}

/// Adds a key's read-only metadata to its representation.
pub(crate) fn decorate(call: &Call, body: &mut Value, meta: Option<&ResourceMeta>) {
    let Some(obj) = body.get_mut("metadata").and_then(Value::as_object_mut) else {
        return;
    };
    obj.insert(
        "created_by".into(),
        Value::from(meta.and_then(|m| m.created_by.clone())),
    );
    obj.insert(
        "disabled_reason".into(),
        Value::from(meta.and_then(|m| m.disabled_reason.clone())),
    );
    let now = call.rt.now();
    let rotated = meta.and_then(|m| m.rotated_at.clone());
    let rotation = meta
        .and_then(|m| m.credential.as_ref())
        .and_then(|c| c.previous.as_ref())
        .filter(|p| UtcMicros::parse_rfc3339(&p.valid_until).is_ok_and(|t| t > now))
        .map_or(Value::Null, |p| {
            let kid = p.imported_digest().is_none().then(|| p.kid.clone());
            json!({"previous_kid": kid, "valid_until": p.valid_until})
        });
    let since = rotated
        .as_deref()
        .or(meta.map(|m| m.created_at.as_str()))
        .and_then(|t| UtcMicros::parse_rfc3339(t).ok());
    let spec = body.get("spec").cloned().unwrap_or(Value::Null);
    let due_at =
        since.and_then(|t| KeyPolicy::of(&call.snap.config.doc, project_of(&spec)).rotation_due_at(t));
    if let Some(obj) = body.get_mut("metadata").and_then(Value::as_object_mut) {
        obj.insert("last_rotated_at".into(), Value::from(rotated));
        obj.insert("rotation".into(), rotation);
        obj.insert(
            "rotation_due".into(),
            Value::Bool(due_at.is_some_and(|due| now >= due)),
        );
        obj.insert(
            "rotation_due_at".into(),
            Value::from(due_at.map(UtcMicros::to_rfc3339_seconds)),
        );
    }
}

/// The requested grace period, else the key policy's `default_grace_period`, else none.
fn grace_period(call: &Call, policy: &KeyPolicy) -> Result<Duration, AdminError> {
    let fallback = policy
        .default_grace_period
        .unwrap_or(Duration::ZERO)
        .min(MAX_GRACE);
    if call.body.is_empty() {
        return Ok(fallback);
    }
    let body = call.json_body()?;
    let invalid = |message: &str| {
        AdminError::validation(vec![ConfigError::new(
            &antirion_config::DocPath::root().key("grace_period"),
            DetailCode::OutOfRange,
            message,
        )])
    };
    match body.get("grace_period") {
        None | Some(Value::Null) => Ok(fallback),
        Some(Value::String(s)) => {
            let d = antirion_config::scalar::parse_duration(s)
                .ok_or_else(|| invalid("grace_period must be a duration"))?;
            if d > MAX_GRACE {
                return Err(invalid("grace_period may not exceed 7d"));
            }
            Ok(d)
        }
        Some(_) => Err(invalid("grace_period must be a duration")),
    }
}

/// `POST /keys/{name}:rotate`: a new secret (new kid) once; the previous one stays valid for the
/// grace period, and a key never has more than two valid secrets.
pub(crate) async fn rotate(call: &Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&["wait"])?;
    let def = model::collection("keys").ok_or_else(AdminError::internal)?;
    let Some((spec, meta)) = call.snap.config.resource(def, name) else {
        return Err(AdminError::not_found());
    };
    let scopes = chain(&call.snap, "keys", name, Some(spec));
    if !call.principal.sees(&scopes) {
        return Err(AdminError::not_found());
    }
    let resource = format!("keys/{name}");
    call.require_owned("keys:rotate", &scopes, owner("keys", Some(spec)))
        .map_err(|e| e.with_resource(resource.clone()))?;
    let grace = grace_period(call, &KeyPolicy::of(&call.snap.config.doc, project_of(spec)))?;
    let Some(current) = meta.and_then(|m| m.credential.clone()) else {
        return Err(AdminError::conflict(
            "conflict",
            "The key's secret is declared in the configuration; change the reference to rotate it.",
            Vec::new(),
        ));
    };
    let cred = Credential::generate(Plane::Data).map_err(|_| AdminError::internal())?;
    let valid_until = UtcMicros(
        call.rt
            .now()
            .0
            .saturating_add(i64::try_from(grace.as_micros()).unwrap_or(i64::MAX)),
    );
    let previous = (!grace.is_zero()).then(|| PreviousCredential {
        kid: current.kid.clone(),
        verifier: current.verifier.clone(),
        valid_until: valid_until.to_rfc3339(),
    });
    let next = KeyCredential {
        kid: cred.kid().to_owned(),
        verifier: call.rt.pepper.verifier(cred.expose()),
        hint: cred.hint(),
        previous: previous.clone(),
    };
    let committed = api_write(call, "keys.rotate", &resource, StatusCode::OK, |state| {
        Ok(Proposal {
            candidate: state.doc.clone(),
            written: Vec::new(),
            credentials: Vec::new(),
            meta_edits: vec![MetaEdit::Credential {
                key: name.to_owned(),
                credential: next.clone(),
            }],
        })
    })
    .await?;
    let mut out = Map::new();
    out.insert("name".into(), Value::from(name));
    out.insert("kid".into(), Value::from(next.kid.clone()));
    out.insert("hint".into(), Value::from(next.hint.clone()));
    out.insert("secret".into(), Value::from(cred.expose()));
    call.audit_read("keys.secret_revealed", &format!("keys/{name}"))
        .await;
    out.insert(
        "previous".into(),
        previous.map_or(Value::Null, |p| {
            let kid = p.imported_digest().is_none().then_some(p.kid);
            json!({"kid": kid, "valid_until": p.valid_until})
        }),
    );
    let resp = call.json(StatusCode::OK, &Value::Object(out));
    Ok(call.mutated(resp, committed.generation).await)
}

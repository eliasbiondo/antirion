//! Whole-document operations: export, the schema, validate, diff and apply.

use antirion_config::{ConfigError, DetailCode, DocPath};
use antirion_identity::rbac::Scope;
use antirion_runtime::changes::{Change, merge_apply};
use antirion_runtime::snapshot::redacted;
use antirion_runtime::{CommitError, Mutation};
use axum::body::Body;
use axum::response::Response;
use http::header::{CONTENT_TYPE, ETAG};
use http::{HeaderValue, Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::commit::finish;
use crate::error::AdminError;

fn source_param(call: &Call) -> Result<String, AdminError> {
    let source = call.param("source").unwrap_or("default");
    let valid = !source.is_empty()
        && source.len() <= 128
        && source.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && source
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if valid {
        Ok(source.to_owned())
    } else {
        Err(AdminError::invalid_parameter("source must be a resource name"))
    }
}

fn incoming(call: &Call) -> Result<Map<String, Value>, AdminError> {
    match call.document()? {
        Value::Object(m) => Ok(m),
        _ => Err(AdminError::malformed(Vec::new())),
    }
}

/// `GET /config`: the dynamic configuration, redacted, as JSON or YAML.
pub(crate) fn export(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    call.require("config:read", &[Scope::Gateway])?;
    let shown = redacted(&call.snap.config.doc, &call.snap.secret_paths);
    let yaml = call.header("accept").is_some_and(|a| a.contains("yaml"));
    let (body, ct) = if yaml {
        (
            antirion_config::emit::to_yaml(&shown).into_bytes(),
            "application/yaml",
        )
    } else {
        (serde_json::to_vec(&shown).unwrap_or_default(), "application/json")
    };
    let mut resp = Response::new(Body::from(body));
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(ct));
    if let Ok(v) = HeaderValue::from_str(&format!("\"{}\"", call.snap.config.generation)) {
        resp.headers_mut().insert(ETAG, v);
    }
    Ok(resp)
}

/// `GET /config/schema`: the published config schema.
pub(crate) fn schema(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    call.require("config:read", &[Scope::Gateway])?;
    let mut resp = Response::new(Body::from(antirion_config::schema::JSON_SCHEMA));
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/schema+json"));
    Ok(resp)
}

/// `POST /config:validate`: always 200, with the errors the resulting configuration would have.
pub(crate) async fn validate(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    call.require("config:read", &[Scope::Gateway])?;
    let mut warnings = Vec::new();
    let errors: Vec<ConfigError> = match incoming(call) {
        Err(e) => e.details.filter(|d| !d.is_empty()).unwrap_or_else(|| {
            vec![ConfigError::new(
                &DocPath::root(),
                DetailCode::MalformedDocument,
                "the document cannot be parsed",
            )]
        }),
        Ok(doc) => {
            let current = &call.snap.config;
            let plan = merge_apply(current, &doc, "validate", false, &call.rt.statics.doc);
            match Box::pin(call.rt.validate_candidate(plan.candidate, current)).await {
                Ok(checked) => {
                    warnings = antirion_runtime::budgets::config::warnings(&checked.doc);
                    Vec::new()
                }
                Err(errors) => errors,
            }
        }
    };
    let errors: Vec<ConfigError> = errors
        .into_iter()
        .map(|mut e| {
            if e.code == DetailCode::DuplicateKey {
                e.code = DetailCode::MalformedDocument;
            }
            e
        })
        .collect();
    Ok(call.json(
        StatusCode::OK,
        &json!({"valid": errors.is_empty(), "errors": errors, "warnings": warnings}),
    ))
}

struct Planned {
    changes: Vec<Change>,
    generation: u64,
    base: u64,
    static_ignored: Vec<String>,
    warnings: Vec<Value>,
}

async fn plan(call: &Call, dry_run: bool, audited: bool) -> Result<Planned, AdminError> {
    let source = source_param(call)?;
    let prune = call.bool_param("prune", false)?;
    let doc = incoming(call)?;
    let expected = match call.header("if-match") {
        None => None,
        Some(v) => Some(
            v.trim()
                .trim_matches('"')
                .parse::<u64>()
                .map_err(|_| AdminError::precondition_failed())?,
        ),
    };
    let base = call.rt.gens.load().generation;
    let mut static_ignored = Vec::new();
    let mut warnings = Vec::new();
    let audit = audited.then(|| call.audit_context("config.apply", "config", StatusCode::OK));
    let statics = &call.rt.statics.doc;
    let result = call
        .rt
        .mutate(|state| {
            if expected.is_some_and(|g| g != state.generation) {
                return Err(AdminError::precondition_failed());
            }
            let p = merge_apply(state, &doc, &source, prune, statics);
            if source == "api"
                && !dry_run
                && !call.approved
                && crate::change_control::governed(&state.doc, &p.candidate)
            {
                return Err(crate::change_control::required());
            }
            static_ignored = p.static_ignored;
            warnings = antirion_runtime::budgets::config::warnings(&p.candidate);
            Ok(Mutation {
                candidate: p.candidate,
                writer: format!("apply:{source}"),
                written: p.written,
                credentials: Vec::new(),
                meta_edits: Vec::new(),
                dry_run,
                source: source.clone(),
                audit,
            })
        })
        .await;
    let committed = if audited {
        finish(call, "config.apply", "config", result).await?
    } else {
        result.map_err(|e| match e {
            CommitError::Rejected(e) => e,
            CommitError::Invalid(d) => AdminError::validation(d),
            CommitError::Store(_) => AdminError::store_unavailable(),
            CommitError::Builder => AdminError::internal(),
        })?
    };
    Ok(Planned {
        changes: committed.changes,
        generation: committed.generation,
        base,
        static_ignored,
        warnings,
    })
}

/// `POST /config:diff`: what apply with the same parameters would do.
pub(crate) async fn diff(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&["source", "prune"])?;
    call.require("config:read", &[Scope::Gateway])?;
    let p = plan(call, true, false).await?;
    Ok(call.json(
        StatusCode::OK,
        &json!({"base_generation": p.base, "changes": p.changes, "static_ignored": p.static_ignored}),
    ))
}

/// `POST /config:apply`.
pub(crate) async fn apply(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&["source", "prune", "dry_run", "wait"])?;
    call.check_wait()?;
    call.require("config:apply", &[Scope::Gateway])
        .map_err(|e| e.with_resource("config"))?;
    let dry_run = call.bool_param("dry_run", false)?;
    let p = plan(call, dry_run, !dry_run).await?;
    let propagated = call.propagated(p.generation).await;
    let resp = call.json(
        StatusCode::OK,
        &json!({"generation": p.generation, "changes": p.changes, "static_ignored": p.static_ignored,
                "propagated": propagated, "warnings": p.warnings}),
    );
    Ok(call.mutated_as(resp, p.generation, propagated))
}

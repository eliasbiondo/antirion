//! Singleton settings (`/settings/<key>`): dynamic keys can be read and written; static keys show the
//! serving node's values and refuse writes.

use antirion_config::model::{self, STATIC_KEYS};
use antirion_identity::rbac::Scope;
use antirion_runtime::changes::Target;
use antirion_runtime::snapshot::redacted;
use antirion_runtime::state::etag_of;
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::commit::{Proposal, api_write, merge_patch};
use crate::error::AdminError;
use crate::perms::setting_permission;

fn representation(
    spec: &Value,
    meta: Option<&antirion_runtime::state::ResourceMeta>,
    generation: u64,
) -> Value {
    json!({
        "spec": spec,
        "metadata": {
            "etag": meta.map_or_else(|| etag_of(spec), |m| m.etag.clone()),
            "generation": meta.map_or(generation, |m| m.generation),
            "created_at": meta.map(|m| m.created_at.clone()),
            "updated_at": meta.map(|m| m.updated_at.clone()),
            "managed_by": meta.map_or("api", |m| m.managed_by.as_str()),
        }
    })
}

/// `POST /settings/smtp:test`: sends one message to `to` with the current settings, now.
pub(crate) async fn smtp_test(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    call.require("security:write", &[antirion_identity::rbac::Scope::Gateway])
        .map_err(|e| e.with_resource("settings/smtp"))?;
    let to = call
        .json_body()?
        .get("to")
        .and_then(Value::as_str)
        .filter(|t| t.len() >= 3)
        .map(str::to_owned)
        .ok_or_else(|| AdminError::invalid_parameter("to must be an email address"))?;
    let Some(settings) = call.snap.identity.console.mail.clone() else {
        return Ok(call.json(
            StatusCode::OK,
            &json!({"ok": false, "status_code": null, "latency_ms": null, "error": "smtp is not enabled"}),
        ));
    };
    let message = antirion_mail::Message {
        from: settings.from.clone(),
        reply_to: settings.reply_to.clone(),
        to,
        subject: "Console mail test".to_owned(),
        body: "This message tests the gateway's outgoing mail settings.\n".to_owned(),
        date: call.rt.now().to_rfc2822(),
    };
    let out = antirion_runtime::operators::Outgoing {
        settings,
        message,
        policy: std::sync::Arc::clone(&call.snap.security.egress),
    };
    let result = antirion_runtime::operators::mail::send_now(&out, &call.rt.metrics).await;
    let body = match &result {
        Ok(latency) => {
            json!({"ok": true, "status_code": null, "latency_ms": latency.as_secs_f64() * 1000.0, "error": null})
        }
        Err(e) => json!({"ok": false, "status_code": null, "latency_ms": null, "error": e.to_string()}),
    };
    call.audit(
        "settings.smtp.test",
        "settings/smtp",
        if result.is_ok() {
            antirion_core::audit::Outcome::Success
        } else {
            antirion_core::audit::Outcome::Failed
        },
        StatusCode::OK,
    )
    .await;
    Ok(call.json(StatusCode::OK, &body))
}

pub(crate) async fn handle(call: &Call, key: &str) -> Result<Response, AdminError> {
    if STATIC_KEYS.contains(&key) {
        return static_setting(call, key).await;
    }
    let Some(def) = model::singleton(key) else {
        return Err(AdminError::not_found());
    };
    match call.method {
        Method::GET => {
            call.allow_params(&[])?;
            call.require(setting_permission(key, false), &[Scope::Gateway])?;
            let shown = redacted(&call.snap.config.doc, &call.snap.secret_paths);
            let spec = model::singleton_value(&shown, def).unwrap_or_else(|| Value::Object(Map::new()));
            let meta = call.snap.config.meta.settings.get(key);
            let mut body = representation(&spec, meta, call.snap.config.generation);
            if key == "auth" {
                body["metadata"]["scim_token_rotated_at"] = crate::scim::rotated_at(&call.rt).await;
            }
            if key == "observability" {
                crate::hints::decorate_observability(&call.snap, &mut body);
            }
            Ok(call.json(StatusCode::OK, &body))
        }
        Method::PUT | Method::PATCH => {
            call.allow_params(&["wait"])?;
            call.check_wait()?;
            call.require(setting_permission(key, true), &[Scope::Gateway])
                .map_err(|e| e.with_resource(format!("settings/{key}")))?;
            if call.method == Method::PATCH {
                let ct = call
                    .header("content-type")
                    .unwrap_or("")
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                if ct != "application/merge-patch+json" && ct != "application/json" {
                    return Err(AdminError::new(
                        StatusCode::UNSUPPORTED_MEDIA_TYPE,
                        "unsupported_media_type",
                        "PATCH takes application/merge-patch+json.",
                    ));
                }
            }
            let body = call.json_body()?;
            let incoming = body
                .get("spec")
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()));
            let patching = call.method == Method::PATCH;
            if key == "governance" {
                let current = model::singleton_value(&call.snap.config.doc, def)
                    .unwrap_or_else(|| Value::Object(Map::new()));
                let mut next = if patching {
                    current.clone()
                } else {
                    Value::Object(Map::new())
                };
                merge_patch(&mut next, &incoming);
                if crate::change_control::removes_audit_sink(&current, &next)
                    && let Some(held) =
                        Box::pin(crate::change_control::second_person(call, "audit_sink_delete")).await?
                {
                    return Ok(held);
                }
            }
            let committed = api_write(
                call,
                &format!("settings.{key}.update"),
                &format!("settings/{key}"),
                StatusCode::OK,
                |state| {
                    let mut spec = if patching {
                        model::singleton_value(&state.doc, def).unwrap_or_else(|| Value::Object(Map::new()))
                    } else {
                        Value::Object(Map::new())
                    };
                    merge_patch(&mut spec, &incoming);
                    let mut doc = state.doc.clone();
                    model::set_singleton(&mut doc, def, Some(spec));
                    Ok(Proposal {
                        candidate: doc,
                        written: vec![Target::Setting(def.name)],
                        credentials: Vec::new(),
                        meta_edits: Vec::new(),
                    })
                },
            )
            .await?;
            let snap = &committed.snapshot;
            let shown = redacted(&snap.config.doc, &snap.secret_paths);
            let spec = model::singleton_value(&shown, def).unwrap_or_else(|| Value::Object(Map::new()));
            let resp = call.json(
                StatusCode::OK,
                &representation(&spec, snap.config.meta.settings.get(key), snap.config.generation),
            );
            Ok(call.mutated(resp, committed.generation).await)
        }
        _ => Err(AdminError::method_not_allowed("GET, PUT, PATCH")),
    }
}

async fn static_setting(call: &Call, key: &str) -> Result<Response, AdminError> {
    if key == "server" && matches!(call.method, Method::PUT | Method::PATCH) {
        return admission_write(call).await;
    }
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    call.require(setting_permission(key, false), &[Scope::Gateway])?;
    let mut spec = call
        .rt
        .statics
        .doc
        .get(key)
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    if key == "server"
        && let (Some(admission), Some(obj)) = (
            call.snap.config.doc.pointer("/server/admission"),
            spec.as_object_mut(),
        )
    {
        obj.insert("admission".into(), admission.clone());
    }
    let shown = antirion_config::secrets::redact(&json!({ key: spec }), &static_secret_paths(key, &spec));
    let spec = shown.get(key).cloned().unwrap_or(Value::Null);
    Ok(call.json(
        StatusCode::OK,
        &representation(&spec, None, call.snap.config.generation),
    ))
}

fn static_secret_paths(key: &str, spec: &Value) -> Vec<antirion_config::DocPath> {
    antirion_config::schema::check_top_level(key, spec).secret_paths
}

/// A write to `server` that changes only `admission`, the one dynamic part of a static key. Any
/// other change to `server` needs a restart, so it is refused like every static write.
async fn admission_write(call: &Call) -> Result<Response, AdminError> {
    call.allow_params(&["wait"])?;
    call.check_wait()?;
    call.require(setting_permission("server", true), &[Scope::Gateway])
        .map_err(|e| e.with_resource("settings/server".to_owned()))?;
    let body = call.json_body()?;
    let spec = body
        .get("spec")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let Some(fields) = spec.as_object() else {
        return Err(AdminError::method_not_allowed("GET"));
    };
    let others_unchanged = fields
        .iter()
        .filter(|(k, _)| k.as_str() != "admission")
        .all(|(k, v)| {
            call.method == Method::PUT && call.rt.statics.doc.pointer(&format!("/server/{k}")) == Some(v)
        });
    let Some(incoming) = fields.get("admission").filter(|_| others_unchanged) else {
        return Err(AdminError::method_not_allowed("GET"));
    };
    let patching = call.method == Method::PATCH;
    let incoming = incoming.clone();
    let committed = api_write(
        call,
        "settings.server.update",
        "settings/server",
        StatusCode::OK,
        |state| {
            let mut admission = if patching {
                state
                    .doc
                    .pointer("/server/admission")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Map::new()))
            } else {
                Value::Object(Map::new())
            };
            merge_patch(&mut admission, &incoming);
            let mut doc = state.doc.clone();
            if let Some(root) = doc.as_object_mut() {
                root.insert("server".into(), json!({ "admission": admission }));
            }
            Ok(Proposal {
                candidate: doc,
                written: Vec::new(),
                credentials: Vec::new(),
                meta_edits: Vec::new(),
            })
        },
    )
    .await?;
    let mut spec = call
        .rt
        .statics
        .doc
        .get("server")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    if let (Some(admission), Some(obj)) = (
        committed.snapshot.config.doc.pointer("/server/admission"),
        spec.as_object_mut(),
    ) {
        obj.insert("admission".into(), admission.clone());
    }
    let resp = call.json(
        StatusCode::OK,
        &representation(&spec, None, committed.snapshot.config.generation),
    );
    Ok(call.mutated(resp, committed.generation).await)
}

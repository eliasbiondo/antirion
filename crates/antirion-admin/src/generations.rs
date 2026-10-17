//! The generation history: `GET /config/generations`, `GET /config/generations/{n}` (secrets
//! redacted) and `POST /config:rollback`, which commits a new generation whose dynamic configuration
//! is that of an earlier one, validated as any change is.

use antirion_core::audit::Outcome;
use antirion_identity::rbac::Scope;
use antirion_runtime::Mutation;
use antirion_runtime::history::redact_sealed;
use axum::body::Body;
use axum::response::Response;
use http::header::{CONTENT_TYPE, ETAG};
use http::{HeaderValue, Method, StatusCode};
use serde_json::{Value, json};

use crate::call::Call;
use crate::commit::finish;
use crate::error::AdminError;
use crate::pagination;

const SCOPE: &str = "generations";

/// `api`, `rollback` or `apply:<source>`, as the history names where a generation came from.
fn source_label(source: &str) -> String {
    match source {
        "api" | "rollback" => source.to_owned(),
        s if s.starts_with("apply:") => s.to_owned(),
        s => format!("apply:{s}"),
    }
}

/// `GET /config/generations`.
pub(crate) async fn list(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&["limit", "cursor"])?;
    call.require("config:read", &[Scope::Gateway])?;
    let (limit, after) = pagination::page_params(call, SCOPE)?;
    let before = after
        .map(|a| {
            a.parse::<u64>()
                .map_err(|_| AdminError::invalid_parameter("the cursor is not valid"))
        })
        .transpose()?;
    let current = call.rt.gens.load().generation;
    let oldest = current.saturating_sub(call.rt.history_limit()).saturating_add(1);
    let rows = call
        .rt
        .store
        .generations(
            before,
            oldest,
            u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1),
        )
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    let more = rows.len() > limit;
    let items: Vec<Value> = rows
        .iter()
        .take(limit)
        .map(|(g, actor_type)| {
            let kind = actor_type
                .clone()
                .unwrap_or_else(|| if g.actor == "system" { "system" } else { "operator" }.to_owned());
            json!({
                "generation": g.generation,
                "created_at": g.created_at,
                "actor": {"type": kind, "id": g.actor},
                "source": source_label(&g.source),
                "summary": summary_words(&g.summary),
            })
        })
        .collect();
    let next = more
        .then(|| {
            rows.get(limit.saturating_sub(1))
                .map(|(g, _)| g.generation.to_string())
        })
        .flatten()
        .map(|p| pagination::encode(&call.rt.cursor_key, SCOPE, &p));
    Ok(call.json(StatusCode::OK, &json!({"items": items, "next_cursor": next})))
}

fn generation_param(text: &str) -> Result<u64, AdminError> {
    text.parse::<u64>()
        .ok()
        .filter(|g| *g >= 1)
        .ok_or_else(AdminError::not_found)
}

/// `GET /config/generations/{n}`.
pub(crate) async fn one(call: &Call, n: &str) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    call.allow_params(&[])?;
    call.require("config:read", &[Scope::Gateway])?;
    let generation = generation_param(n)?;
    let mut doc = call
        .rt
        .kept_generation(generation)
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .ok_or_else(AdminError::not_found)?;
    redact_sealed(&mut doc);
    if let Some(root) = doc.as_object_mut() {
        root.remove("version");
    }
    let yaml = call.header("accept").is_some_and(|a| a.contains("yaml"));
    let (body, ct) = if yaml {
        (
            antirion_config::emit::to_yaml(&doc).into_bytes(),
            "application/yaml",
        )
    } else {
        (serde_json::to_vec(&doc).unwrap_or_default(), "application/json")
    };
    let mut resp = Response::new(Body::from(body));
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(ct));
    if let Ok(v) = HeaderValue::from_str(&format!("\"{generation}\"")) {
        resp.headers_mut().insert(ETAG, v);
    }
    Ok(resp)
}

/// `POST /config:rollback`.
pub(crate) async fn rollback(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&["wait"])?;
    call.check_wait()?;
    call.require("config:apply", &[Scope::Gateway])
        .map_err(|e| e.with_resource("config"))?;
    let body = call.json_body()?;
    let generation = body
        .get("generation")
        .and_then(Value::as_u64)
        .filter(|g| *g >= 1)
        .ok_or_else(|| {
            AdminError::validation(vec![antirion_config::ConfigError::new(
                &antirion_config::DocPath::root().key("generation"),
                antirion_config::DetailCode::Required,
                "generation must be an integer of at least 1",
            )])
        })?;
    let expected = match call.header("if-match") {
        None => None,
        Some(v) => Some(
            v.trim()
                .trim_matches('"')
                .parse::<u64>()
                .map_err(|_| AdminError::precondition_failed())?,
        ),
    };
    let doc = call
        .rt
        .opened_generation(generation)
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .ok_or_else(AdminError::not_found)?;
    let warnings = antirion_runtime::budgets::config::warnings(&doc);
    let audit = call.audit_context("config.rollback", "config", StatusCode::OK);
    let result = call
        .rt
        .mutate(|state| {
            if expected.is_some_and(|g| g != state.generation) {
                return Err(AdminError::precondition_failed());
            }
            if !call.approved && crate::change_control::governed(&state.doc, &doc) {
                return Err(crate::change_control::required());
            }
            Ok(Mutation {
                candidate: doc,
                writer: "rollback".to_owned(),
                written: Vec::new(),
                credentials: Vec::new(),
                meta_edits: Vec::new(),
                dry_run: false,
                source: "rollback".to_owned(),
                audit: Some(audit),
            })
        })
        .await;
    let committed = finish(call, "config.rollback", "config", result).await?;
    let propagated = call.propagated(committed.generation).await;
    let resp = call.json(
        StatusCode::OK,
        &json!({"generation": committed.generation, "changes": committed.changes, "static_ignored": [],
                "propagated": propagated, "warnings": warnings}),
    );
    if committed.changes.is_empty() {
        call.audit("config.rollback", "config", Outcome::Success, StatusCode::OK)
            .await;
    }
    Ok(call.mutated_as(resp, committed.generation, propagated))
}

/// A generation's summary in today's words: older versions stored `1 change(s)`.
fn summary_words(summary: &str) -> String {
    match summary
        .strip_suffix(" change(s)")
        .and_then(|n| n.parse::<u64>().ok())
    {
        Some(1) => "1 change".to_owned(),
        Some(n) => format!("{n} changes"),
        None => summary.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_summaries_read_in_todays_words() {
        assert_eq!(summary_words("1 change(s)"), "1 change");
        assert_eq!(summary_words("3 change(s)"), "3 changes");
        assert_eq!(summary_words("2 changes"), "2 changes");
        assert_eq!(summary_words("rollback to 4"), "rollback to 4");
    }

    #[test]
    fn sources_name_where_a_generation_came_from() {
        assert_eq!(source_label("api"), "api");
        assert_eq!(source_label("rollback"), "rollback");
        assert_eq!(source_label("file"), "apply:file");
        assert_eq!(source_label("apply:ci"), "apply:ci");
    }
}

//! Collection resources: list, create, read, create-or-replace, merge-patch and delete, each one a
//! whole-configuration change validated and committed as one generation.

use antirion_config::model::{self, CollectionDef, DEFAULT_TENANT};
use antirion_config::{ConfigError, DetailCode, DocPath, semantic};
use antirion_identity::{Credential, Plane};
use antirion_runtime::changes::Target;
use antirion_runtime::snapshot::redacted;
use antirion_runtime::state::{ConfigState, KeyCredential, ResourceMeta, etag_of};
use axum::response::Response;
use http::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::commit::{Proposal, api_write, merge_patch};
use crate::error::AdminError;
use crate::perms::{Verb, chain, is_tenancy, moved, owner, permission};
use crate::tenancy::{descendants, disable_owned_keys, implicit_default};
use crate::{keys, pagination};

/// The resource representation: `{name, spec, metadata}`.
pub(crate) fn representation(
    name: &str,
    spec: &Value,
    meta: Option<&ResourceMeta>,
    generation: u64,
) -> Value {
    let mut metadata = json!({
        "etag": meta.map_or_else(|| etag_of(spec), |m| m.etag.clone()),
        "generation": meta.map_or(generation, |m| m.generation),
        "created_at": meta.map(|m| m.created_at.clone()),
        "updated_at": meta.map(|m| m.updated_at.clone()),
        "managed_by": meta.map_or("api", |m| m.managed_by.as_str()),
    });
    if let (Some(cred), Some(obj)) = (meta.and_then(|m| m.credential.as_ref()), metadata.as_object_mut()) {
        let kid = (cred.imported_digest().is_none()).then(|| cred.kid.clone());
        obj.insert("kid".into(), Value::from(kid));
        obj.insert("hint".into(), Value::from(cred.hint.clone()));
    }
    json!({"name": name, "spec": spec, "metadata": metadata})
}

fn shown_spec(call: &Call, c: &CollectionDef, name: &str) -> Option<Value> {
    let shown = redacted(&call.snap.config.doc, &call.snap.secret_paths);
    model::collection_map(&shown, c)?.get(name).cloned()
}

fn resource_name(c: &CollectionDef, name: &str) -> String {
    format!("{}/{name}", c.name)
}

fn etag_response(call: &Call, status: StatusCode, body: &Value) -> Response {
    let mut resp = call.json(status, body);
    if let Some(etag) = body.pointer("/metadata/etag").and_then(Value::as_str)
        && let Ok(v) = http::HeaderValue::from_str(etag)
    {
        resp.headers_mut().insert(http::header::ETAG, v);
    }
    resp
}

/// `GET` and `POST` on a collection.
pub(crate) async fn collection(call: &Call, c: &'static CollectionDef) -> Result<Response, AdminError> {
    match call.method {
        Method::GET => Box::pin(list(call, c)).await,
        Method::POST => Box::pin(create(call, c)).await,
        _ => Err(AdminError::method_not_allowed("GET, POST")),
    }
}

/// `GET`, `PUT`, `PATCH` and `DELETE` on one resource.
pub(crate) async fn resource(
    call: &Call,
    c: &'static CollectionDef,
    name: &str,
) -> Result<Response, AdminError> {
    match call.method {
        Method::GET => Box::pin(read(call, c, name)).await,
        Method::PUT => Box::pin(put(call, c, name)).await,
        Method::PATCH => Box::pin(patch(call, c, name)).await,
        Method::DELETE => Box::pin(delete(call, c, name)).await,
        _ => Err(AdminError::method_not_allowed("GET, PUT, PATCH, DELETE")),
    }
}

async fn list(call: &Call, c: &'static CollectionDef) -> Result<Response, AdminError> {
    let people = crate::people::Filter::from_call(call, c)?;
    let filter = if c.name == "models" {
        Some(crate::models::ListFilter::from_call(call)?)
    } else {
        if people.is_none() {
            call.allow_params(&["limit", "cursor"])?;
        }
        None
    };
    let facts = crate::people::Facts::load(call, c.name).await;
    let (limit, after) = pagination::page_params(call, c.name)?;
    let read_perm = permission(c.name, Verb::Read);
    if !is_tenancy(c.name) {
        call.require(&read_perm, &[antirion_identity::rbac::Scope::Gateway])?;
    }
    let state = &call.snap.config;
    let shown = redacted(&state.doc, &call.snap.secret_paths);
    let mut map = model::collection_map(&shown, c).cloned().unwrap_or_default();
    if let Some(default) = implicit_default(c.name, DEFAULT_TENANT) {
        map.entry(DEFAULT_TENANT.to_owned()).or_insert(default);
    }
    map.sort_keys();
    if let Some(f) = &filter {
        map.retain(|_, spec| f.keeps(spec));
    }
    let people_counts = match (&people, &facts) {
        (Some(p), Some(facts)) => Some(p.select(call, facts, c.name, &mut map, |name, spec| {
            call.principal.allows_owned(
                &read_perm,
                &chain(&call.snap, c.name, name, Some(spec)),
                owner(c.name, Some(spec)),
            )
        })),
        _ => None,
    };
    let metas = state.meta.resources.get(c.name);
    let mut items = Vec::new();
    let mut next = None;
    for (name, spec) in &map {
        if after.as_ref().is_some_and(|a| name.as_str() <= a.as_str()) {
            continue;
        }
        if is_tenancy(c.name)
            && !call.principal.allows_owned(
                &read_perm,
                &chain(&call.snap, c.name, name, Some(spec)),
                owner(c.name, Some(spec)),
            )
        {
            continue;
        }
        if items.len() == limit {
            next = items
                .last()
                .and_then(|i: &Value| i.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            break;
        }
        let meta = metas.and_then(|m| m.get(name));
        let mut item = representation(name, spec, meta, state.generation);
        if c.name == "keys" {
            keys::decorate(call, &mut item, meta);
        }
        if c.name == "providers" {
            crate::hints::decorate(&call.snap, name, &mut item);
            crate::provider_ops::decorate(&call.snap, name, &mut item, call.rt.now());
        }
        if c.name == "models" {
            crate::models::decorate(call, name, &mut item);
        }
        if c.name == "deployments" {
            crate::regions::decorate(call, name, &mut item);
        }
        if let Some(facts) = &facts {
            facts.decorate(call, c.name, name, &mut item, meta);
        }
        items.push(item);
    }
    let next_cursor = next.map(|n| pagination::encode(&call.rt.cursor_key, c.name, &n));
    let mut body = json!({"items": items, "next_cursor": next_cursor});
    if let (Some(f), Some(o)) = (&filter, body.as_object_mut()) {
        o.insert("counts".into(), f.counts(map.values()));
    }
    if let (Some(counts), Some(o)) = (people_counts, body.as_object_mut()) {
        o.insert("counts".into(), counts);
    }
    Ok(call.json(StatusCode::OK, &body))
}

/// Checks visibility and `permission` on an existing resource; outside the caller's scope it is
/// reported exactly like a missing one.
fn authorize_existing(
    call: &Call,
    c: &CollectionDef,
    name: &str,
    spec: &Value,
    verb: Verb,
) -> Result<(), AdminError> {
    let scopes = chain(&call.snap, c.name, name, Some(spec));
    if is_tenancy(c.name) && !call.principal.sees(&scopes) {
        return Err(AdminError::not_found());
    }
    call.require_owned(&permission(c.name, verb), &scopes, owner(c.name, Some(spec)))
}

/// An update that moves a resource under another parent also needs, at the destination, the
/// permission that would create it there.
fn authorize_move(
    call: &Call,
    c: &CollectionDef,
    name: &str,
    old: &Value,
    new: &Value,
) -> Result<(), AdminError> {
    let Some(to) = moved(&call.snap, c.name, name, old, new) else {
        return Ok(());
    };
    let needed = permission(c.name, Verb::Create);
    if call
        .principal
        .allows_owned(&needed, &to, owner(c.name, Some(new)))
    {
        return Ok(());
    }
    let parent = to
        .iter()
        .rev()
        .nth(1)
        .map_or_else(|| "gateway".to_owned(), ToString::to_string);
    Err(AdminError::new(
        StatusCode::FORBIDDEN,
        "forbidden",
        format!(
            "Moving {} under {parent} needs the {needed} permission there.",
            resource_name(c, name)
        ),
    )
    .with_resource(resource_name(c, name)))
}

async fn read(call: &Call, c: &'static CollectionDef, name: &str) -> Result<Response, AdminError> {
    call.allow_params(&[])?;
    let state = &call.snap.config;
    let Some((spec, meta)) = state.resource(c, name) else {
        if let Some(default) = implicit_default(c.name, name) {
            authorize_existing(call, c, name, &default, Verb::Read)?;
            return Ok(etag_response(
                call,
                StatusCode::OK,
                &representation(name, &default, None, state.generation),
            ));
        }
        if !is_tenancy(c.name) {
            call.require(
                &permission(c.name, Verb::Read),
                &[antirion_identity::rbac::Scope::Gateway],
            )?;
        }
        return Err(AdminError::not_found());
    };
    authorize_existing(call, c, name, spec, Verb::Read)?;
    let shown = shown_spec(call, c, name).unwrap_or(Value::Null);
    let mut body = representation(name, &shown, meta, state.generation);
    if c.name == "keys" {
        keys::decorate(call, &mut body, meta);
    }
    if c.name == "providers" {
        crate::hints::decorate(&call.snap, name, &mut body);
        crate::provider_ops::decorate(&call.snap, name, &mut body, call.rt.now());
    }
    if c.name == "models" {
        crate::models::decorate(call, name, &mut body);
    }
    if c.name == "deployments" {
        crate::regions::decorate(call, name, &mut body);
    }
    if c.name == "organizations" {
        Box::pin(crate::org_deletion::decorate(call, name, &mut body)).await;
    }
    if let Some(facts) = crate::people::Facts::load(call, c.name).await {
        facts.decorate(call, c.name, name, &mut body, meta);
    }
    Ok(etag_response(call, StatusCode::OK, &body))
}

fn body_spec(c: &CollectionDef, name: &str, body: &Map<String, Value>) -> Result<Value, AdminError> {
    match body.get("spec") {
        None => Ok(Value::Object(Map::new())),
        Some(v @ Value::Object(_)) => Ok(v.clone()),
        Some(_) => Err(AdminError::validation(vec![ConfigError::new(
            &c.resource_path(name),
            DetailCode::TypeMismatch,
            "spec must be an object",
        )])),
    }
}

/// A new key's credential and the plaintext secret to return once: generated, or imported from a
/// legacy key's digest (no plaintext then).
fn new_credential(
    call: &Call,
    c: &CollectionDef,
    spec: &Value,
    import: Option<&Value>,
) -> Result<Option<(KeyCredential, Option<String>)>, AdminError> {
    if c.name != "keys" {
        return Ok(None);
    }
    if let Some(import) = import {
        return keys::imported(call, spec, import).map(|k| Some((k, None)));
    }
    if spec.get("secret").is_some() || spec.get("kind").and_then(Value::as_str) == Some("jwt") {
        return Ok(None);
    }
    let cred = Credential::generate(Plane::Data).map_err(|_| AdminError::internal())?;
    let record = KeyCredential {
        kid: cred.kid().to_owned(),
        verifier: call.rt.pepper.verifier(cred.expose()),
        hint: cred.hint(),
        previous: None,
    };
    Ok(Some((record, Some(cred.expose().to_owned()))))
}

/// Keys cannot change project, kind or secret after creation.
fn key_immutables(c: &CollectionDef, name: &str, old: &Value, new: &Value) -> Result<(), AdminError> {
    if c.name != "keys" {
        return Ok(());
    }
    let mut details = Vec::new();
    for field in ["project", "kind", "secret"] {
        let default = (field == "project").then(|| Value::from(DEFAULT_TENANT));
        let before = old.get(field).cloned().or_else(|| default.clone());
        let after = new.get(field).cloned().or(default);
        if before != after
            && after.as_ref().and_then(Value::as_str) != Some(antirion_config::secrets::REDACTED)
        {
            details.push(ConfigError::new(
                &c.resource_path(name).key(field),
                DetailCode::Conflict,
                format!("{field} cannot be changed"),
            ));
        }
    }
    if details.is_empty() {
        Ok(())
    } else {
        Err(AdminError::validation(details))
    }
}

fn with_resource(state: &ConfigState, c: &CollectionDef, name: &str, spec: Value) -> Value {
    let mut doc = state.doc.clone();
    if let Some(map) = model::collection_map_mut(&mut doc, c) {
        map.insert(name.to_owned(), spec);
    }
    doc
}

async fn write_and_render(
    call: &Call,
    c: &'static CollectionDef,
    name: &str,
    spec: Value,
    verb: Verb,
    credential: Option<(KeyCredential, Option<String>)>,
) -> Result<Response, AdminError> {
    let action = format!(
        "{}.{}",
        c.name.replace('/', "."),
        if verb == Verb::Create { "create" } else { "update" }
    );
    let status = if verb == Verb::Create {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let (cred, plaintext) = credential.map_or((None, None), |(c, p)| (Some(c), p));
    let resource = resource_name(c, name);
    if c.name == "users"
        && crate::users_cap::activates(call.snap.config.resource(c, name).map(|(s, _)| s), &spec)
    {
        crate::users_cap::check(&call.rt)
            .await
            .map_err(|e| e.with_resource(resource.clone()))?;
    }
    let committed = api_write(call, &action, &resource, status, |state| {
        Ok(Proposal {
            candidate: with_resource(state, c, name, spec),
            written: vec![Target::Resource(c.name, name.to_owned())],
            credentials: cred.map(|k| vec![(name.to_owned(), k)]).unwrap_or_default(),
            meta_edits: Vec::new(),
        })
    })
    .await?;
    let snap = &committed.snapshot;
    let shown = redacted(&snap.config.doc, &snap.secret_paths);
    let spec_shown = model::collection_map(&shown, c)
        .and_then(|m| m.get(name))
        .cloned()
        .unwrap_or(Value::Null);
    let meta = snap.config.meta.resources.get(c.name).and_then(|m| m.get(name));
    let mut body = representation(name, &spec_shown, meta, snap.config.generation);
    if c.name == "keys" {
        keys::decorate(call, &mut body, meta);
    }
    if c.name == "providers" {
        crate::hints::decorate(snap, name, &mut body);
    }
    if let Some(facts) = crate::people::Facts::load(call, c.name).await {
        facts.decorate(call, c.name, name, &mut body, meta);
    }
    if let (Some(secret), Some(obj)) = (plaintext, body.as_object_mut()) {
        obj.insert("secret".into(), Value::from(secret));
        call.audit_read("keys.secret_revealed", &resource_name(c, name))
            .await;
    }
    let resp = etag_response(call, status, &body);
    let resp = call.mutated(resp, committed.generation).await;
    Ok(if verb == Verb::Create {
        call.created(resp, &format!("/admin/v1/{}/{name}", c.name))
    } else {
        resp
    })
}

async fn create(call: &Call, c: &'static CollectionDef) -> Result<Response, AdminError> {
    call.allow_params(&["wait"])?;
    call.check_wait()?;
    let body = call.json_body()?;
    let name = match body.get("name").and_then(Value::as_str) {
        Some(n) if !n.is_empty() => n.to_owned(),
        _ => {
            let path = DocPath::parse_dotted(&c.dotted());
            return Err(AdminError::validation(vec![ConfigError::new(
                &path,
                DetailCode::Required,
                "name is required",
            )]));
        }
    };
    let mut spec = body_spec(c, &name, &body)?;
    call.require_owned(
        &permission(c.name, Verb::Create),
        &chain(&call.snap, c.name, &name, Some(&spec)),
        owner(c.name, Some(&spec)),
    )
    .map_err(|e| e.with_resource(resource_name(c, &name)))?;
    if c.name == "keys" {
        keys::prepare_new(call, &name, &mut spec).map_err(|e| e.with_resource(resource_name(c, &name)))?;
    }
    let exists = call.snap.config.resource(c, &name).is_some()
        || (matches!(c.name, "organizations" | "teams" | "projects") && name == DEFAULT_TENANT);
    if exists {
        return Err(AdminError::conflict(
            "already_exists",
            "A resource with this name already exists.",
            Vec::new(),
        ));
    }
    let credential = new_credential(call, c, &spec, body.get("import"))?;
    write_and_render(call, c, &name, spec, Verb::Create, credential).await
}

fn check_if_match(call: &Call, current_etag: Option<&str>) -> Result<(), AdminError> {
    if let Some(expected) = call.header("if-match") {
        if expected.trim() != "*" && Some(expected.trim()) != current_etag {
            return Err(AdminError::precondition_failed());
        }
        if current_etag.is_none() {
            return Err(AdminError::precondition_failed());
        }
    }
    Ok(())
}

async fn put(call: &Call, c: &'static CollectionDef, name: &str) -> Result<Response, AdminError> {
    call.allow_params(&["wait"])?;
    call.check_wait()?;
    let body = call.json_body()?;
    let mut spec = body_spec(c, name, &body)?;
    let existing = call
        .snap
        .config
        .resource(c, name)
        .map(|(s, m)| (s.clone(), m.map(|m| m.etag.clone())));
    match &existing {
        Some((old, etag)) => {
            authorize_existing(call, c, name, old, Verb::Update)?;
            if call.header("if-none-match").is_some_and(|v| v.trim() == "*") {
                return Err(AdminError::precondition_failed());
            }
            check_if_match(call, etag.as_deref())?;
            key_immutables(c, name, old, &spec)?;
            authorize_move(call, c, name, old, &spec)?;
            if c.name == "keys" {
                keys::check_update(call, name, old, &spec)?;
            }
            write_and_render(call, c, name, spec, Verb::Update, None).await
        }
        None => {
            call.require_owned(
                &permission(c.name, Verb::Create),
                &chain(&call.snap, c.name, name, Some(&spec)),
                owner(c.name, Some(&spec)),
            )?;
            check_if_match(call, None)?;
            if c.name == "keys" {
                keys::prepare_new(call, name, &mut spec)?;
            }
            let credential = new_credential(call, c, &spec, None)?;
            write_and_render(call, c, name, spec, Verb::Create, credential).await
        }
    }
}

async fn patch(call: &Call, c: &'static CollectionDef, name: &str) -> Result<Response, AdminError> {
    call.allow_params(&["wait"])?;
    call.check_wait()?;
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
    let implicit = implicit_default(c.name, name);
    let (old, meta) = match (call.snap.config.resource(c, name), &implicit) {
        (Some(found), _) => found,
        (None, Some(default)) => (default, None),
        (None, None) => {
            if !is_tenancy(c.name) {
                call.require(
                    &permission(c.name, Verb::Update),
                    &[antirion_identity::rbac::Scope::Gateway],
                )?;
            }
            return Err(AdminError::not_found());
        }
    };
    authorize_existing(call, c, name, old, Verb::Update)
        .map_err(|e| e.with_resource(resource_name(c, name)))?;
    // The implicit default tenancy resource has no stored metadata; its ETag is the one a read
    // answers for it, so a read-then-patch round trip succeeds.
    let implicit_etag = meta.is_none().then(|| etag_of(old));
    check_if_match(call, meta.map(|m| m.etag.as_str()).or(implicit_etag.as_deref()))?;
    let body = call.json_body()?;
    let mut spec = old.clone();
    if let Some(p) = body.get("spec") {
        merge_patch(&mut spec, p);
    }
    key_immutables(c, name, old, &spec)?;
    authorize_move(call, c, name, old, &spec)?;
    if c.name == "keys" {
        keys::check_update(call, name, old, &spec).map_err(|e| e.with_resource(resource_name(c, name)))?;
    }
    write_and_render(call, c, name, spec, Verb::Update, None).await
}

async fn delete(call: &Call, c: &'static CollectionDef, name: &str) -> Result<Response, AdminError> {
    let cascadable = matches!(c.name, "organizations" | "teams" | "projects");
    call.allow_params(if cascadable {
        &["wait", "cascade"]
    } else {
        &["wait"]
    })?;
    call.check_wait()?;
    let cascade = cascadable && call.bool_param("cascade", false)?;
    if let Some(default) = implicit_default(c.name, name) {
        authorize_existing(call, c, name, &default, Verb::Delete)?;
        return Err(AdminError::conflict(
            "resource_in_use",
            "The default tenancy resources cannot be deleted.",
            Vec::new(),
        ));
    }
    let Some((old, meta)) = call.snap.config.resource(c, name) else {
        if !is_tenancy(c.name) {
            call.require(
                &permission(c.name, Verb::Delete),
                &[antirion_identity::rbac::Scope::Gateway],
            )?;
        }
        return Err(AdminError::not_found());
    };
    authorize_existing(call, c, name, old, Verb::Delete)?;
    check_if_match(call, meta.map(|m| m.etag.as_str()))?;
    let resource = resource_name(c, name);
    let action = format!("{}.delete", c.name.replace('/', "."));
    let committed = api_write(call, &action, &resource, StatusCode::NO_CONTENT, |state| {
        let mut doc = state.doc.clone();
        let mut removed: Vec<(&'static str, String)> = vec![(c.name, name.to_owned())];
        if cascade {
            removed.extend(descendants(&doc, c, name));
        }
        let (disabled, meta_edits) = if c.name == "users" {
            disable_owned_keys(&mut doc, name)
        } else {
            (Vec::new(), Vec::new())
        };
        for (coll, n) in &removed {
            if let Some(map) = model::collection(coll).and_then(|cd| model::collection_map_mut(&mut doc, cd))
            {
                map.remove(n);
            }
        }
        let in_use: Vec<ConfigError> = semantic::references(&doc)
            .into_iter()
            .filter(|r| {
                removed
                    .iter()
                    .any(|(coll, n)| *coll == r.collection && *n == r.name)
            })
            .map(|r| {
                ConfigError::new(
                    &r.from,
                    DetailCode::Conflict,
                    format!("references {}/{}", r.collection, r.name),
                )
            })
            .collect();
        if !in_use.is_empty() {
            return Err(AdminError::conflict(
                "resource_in_use",
                "Other resources reference this resource.",
                in_use,
            ));
        }
        Ok(Proposal {
            candidate: doc,
            written: removed
                .iter()
                .map(|(coll, n)| Target::Resource(coll, n.clone()))
                .chain(disabled.into_iter().map(|k| Target::Resource("keys", k)))
                .collect(),
            credentials: Vec::new(),
            meta_edits,
        })
    })
    .await?;
    if c.name == "users" {
        crate::users::forget_user(&call.rt, name).await;
    }
    let resp = call.no_content();
    Ok(call.mutated(resp, committed.generation).await)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn representation_shape() {
        let r = representation("m", &json!({"target": "d"}), None, 3);
        assert_eq!(r["name"], "m");
        assert_eq!(r["metadata"]["generation"], 3);
        assert!(r["metadata"]["etag"].as_str().unwrap().starts_with('"'));
    }
}

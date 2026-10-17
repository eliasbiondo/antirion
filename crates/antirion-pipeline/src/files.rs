//! The Files API. An upload goes to one provider (the request's `x-antirion-files-provider`, else
//! `api.endpoints.files.default_provider`, else the one OpenAI-dialect provider the key may use)
//! and is answered with a gateway file id that maps to the project, the provider and the provider's
//! id. Reads, deletes and downloads resolve that mapping, so a file is reachable only from its
//! project and only on its provider; a gateway id inside an inference body is replaced with the
//! provider's and confines routing to that provider.

use std::sync::Arc;

use antirion_core::Stage;
use antirion_dialects::Endpoint;
use antirion_egress::Payload;
use antirion_error::{GatewayError, Message};
use antirion_resilience::RequestKind;
use antirion_runtime::identity::KeyRt;
use antirion_store::UpstreamObject;
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::request::Parts;
use http::{HeaderValue, Method, Response, StatusCode};
use hyper::body::Incoming;
use serde_json::{Value, json};

use crate::account::{self, Admission, End};
use crate::auth::Authenticated;
use crate::ctx::{Body, Ctx, full, streamed};
use crate::parse::Parsed;
use crate::provider_call::{self, AnswerBody, Answered, Call};
use crate::reasons;

/// The prefix of a gateway file id.
pub(crate) const FILE_PREFIX: &str = "file-ant_";

const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// A new gateway id: `prefix` and 26 base32 characters of a random 128-bit value.
pub(crate) fn new_id(prefix: &str) -> String {
    let v = antirion_core::ResourceUid::random().0;
    let mut out = String::with_capacity(prefix.len() + 26);
    out.push_str(prefix);
    for i in (0..26).rev() {
        let digit = usize::try_from((v >> (i * 5)) & 31).unwrap_or(0);
        out.push(char::from(BASE32.get(digit).copied().unwrap_or(b'a')));
    }
    out
}

/// Whether `id` has the shape of a gateway id with `prefix`.
pub(crate) fn gateway_id(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix)
        .is_some_and(|rest| rest.len() == 26 && rest.bytes().all(|b| BASE32.contains(&b)))
}

pub(crate) fn file_not_found() -> GatewayError {
    GatewayError::with_reason(
        reasons::FILE_NOT_FOUND,
        Stage::Resolve,
        Message::Fixed("No such file."),
    )
    .param("file_id")
}

pub(crate) fn store_unavailable() -> GatewayError {
    let mut e = GatewayError::with_reason(
        reasons::STORE_UNAVAILABLE,
        Stage::Resolve,
        Message::Fixed("The gateway's store is unavailable."),
    );
    e.retry_after = Some(std::time::Duration::from_secs(1));
    e
}

/// A JSON response.
pub(crate) fn json_response(cx: &Ctx, status: StatusCode, v: &Value) -> Response<Body> {
    let mut resp = Response::new(full(Bytes::from(serde_json::to_vec(v).unwrap_or_default())));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    cx.finish(&mut resp, false);
    resp
}

/// Records the request as uncacheable when a response cache applies to it: files, batches and
/// their reads are never cached.
pub(crate) fn uncacheable(cx: &Ctx, key: &KeyRt) {
    let policy = crate::policy::effective(&cx.snap, "cache", key, "", None);
    if antirion_cache::CachePolicy::enabled_in(&policy) {
        crate::observe::cache_lookup(cx, "uncacheable", None);
    }
}

/// The effective tags of a request: the tenancy chain's, the key's and the request's.
pub(crate) fn tags(cx: &Ctx, auth: &Authenticated) -> Vec<String> {
    let key = &auth.key;
    cx.snap
        .identity
        .chain(key)
        .iter()
        .flatten()
        .flat_map(|t| t.tags.iter().cloned())
        .chain(key.tags.iter().cloned())
        .chain(auth.controls.tags.iter().cloned())
        .collect()
}

/// Whether the key may use `provider` for an operation class: one of its usable deployments that
/// declares the class is allowed to the key.
fn provider_allowed(cx: &Ctx, key: &KeyRt, provider: &str, class: &str) -> bool {
    provider_call::carrier(cx, key, provider, class).is_some()
}

fn no_allowed_provider(name: &str) -> GatewayError {
    GatewayError::with_reason(
        antirion_identity::reasons::NO_ALLOWED_DEPLOYMENT,
        Stage::Resolve,
        Message::built(format!("This key may not use provider `{name}` for files.")),
    )
}

/// The provider of an upload: the header, else the configured default, else the only one.
fn choose_provider(cx: &Ctx, parts: &Parts, key: &KeyRt) -> Result<String, GatewayError> {
    let header = parts
        .headers
        .get(antirion_core::headers::FILES_PROVIDER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let configured = cx
        .snap
        .config
        .doc
        .pointer("/api/endpoints/files/default_provider")
        .and_then(Value::as_str);
    if let Some(name) = header.or(configured) {
        return if provider_allowed(cx, key, name, "files") {
            Ok(name.to_owned())
        } else {
            Err(no_allowed_provider(name))
        };
    }
    let mut names: Vec<&String> = cx
        .snap
        .providers
        .providers
        .iter()
        .filter(|(name, p)| p.kind.openai_family() && provider_allowed(cx, key, name, "files"))
        .map(|(name, _)| name)
        .collect();
    names.sort();
    match names.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(GatewayError::with_reason(
            antirion_identity::reasons::NO_ALLOWED_DEPLOYMENT,
            Stage::Resolve,
            Message::Fixed("This key may use no provider that stores files."),
        )),
        _ => Err(GatewayError::with_reason(
            reasons::FILES_PROVIDER_REQUIRED,
            Stage::Resolve,
            Message::Fixed(
                "More than one provider can store this file: name one in the x-antirion-files-provider header.",
            ),
        )),
    }
}

/// The client's view of a provider object: its id replaced with the gateway's.
fn shown(mut v: Value, id: &str) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.insert("id".into(), Value::from(id));
    }
    v
}

/// A mapped object of `kind` that belongs to the key's project.
pub(crate) async fn owned(
    cx: &Ctx,
    kind: &str,
    id: &str,
    project: &str,
) -> Result<Option<UpstreamObject>, GatewayError> {
    let prefix = if kind == "batch" {
        crate::batches::BATCH_PREFIX
    } else {
        FILE_PREFIX
    };
    if !gateway_id(id, prefix) {
        return Ok(None);
    }
    match cx.rt.store.object(kind, id).await {
        Ok(Some(o)) if o.project == project => Ok(Some(o)),
        Ok(_) => Ok(None),
        Err(_) => Err(store_unavailable()),
    }
}

/// Maps a provider's file id to a gateway id (one is created the first time it is seen, for files
/// the provider made: batch outputs and errors).
pub(crate) async fn map_upstream_file(
    cx: &Ctx,
    project: &str,
    provider: &str,
    upstream_id: &str,
) -> Result<String, GatewayError> {
    match cx
        .rt
        .store
        .object_by_upstream("file", provider, upstream_id)
        .await
    {
        Ok(Some(o)) if o.project == project => return Ok(o.id),
        Ok(_) => {}
        Err(_) => return Err(store_unavailable()),
    }
    let id = new_id(FILE_PREFIX);
    let obj = UpstreamObject {
        kind: "file".into(),
        id: id.clone(),
        project: project.to_owned(),
        provider: provider.to_owned(),
        upstream_id: upstream_id.to_owned(),
        created_us: cx.rt.now().0,
        body: json!({"id": upstream_id}).to_string(),
    };
    cx.rt
        .store
        .put_object(obj)
        .await
        .map_err(|_| store_unavailable())?;
    Ok(id)
}

/// Serves a Files endpoint.
pub(crate) async fn serve(
    cx: &mut Ctx,
    parts: &Parts,
    incoming: Incoming,
    endpoint: &Endpoint,
    auth: &Authenticated,
) -> Response<Body> {
    let id = endpoint.path_param.clone();
    uncacheable(cx, &auth.key);
    let result = match (&parts.method, id.as_deref(), endpoint.action) {
        (&Method::POST, None, None) => upload(cx, parts, incoming, endpoint, auth).await,
        (&Method::GET, None, None) => list(cx, parts, &auth.key).await,
        (m, Some(id), action) => object(cx, parts, endpoint, &auth.key, m, id, action).await,
        _ => Err(crate::accept::not_found(
            antirion_error::reason::hygiene::UNKNOWN_ENDPOINT,
        )),
    };
    let mut resp = match result {
        Ok(r) => r,
        Err(e) => cx.error(&e),
    };
    crate::cache::decorate(cx, &mut resp);
    resp
}

async fn upload(
    cx: &mut Ctx,
    parts: &Parts,
    incoming: Incoming,
    endpoint: &Endpoint,
    auth: &Authenticated,
) -> Result<Response<Body>, GatewayError> {
    let key = Arc::clone(&auth.key);
    let content_type = parts.headers.get(CONTENT_TYPE).cloned();
    let Some(boundary) = content_type
        .as_ref()
        .and_then(|v| v.to_str().ok())
        .and_then(antirion_wire::multipart::form_data_boundary)
    else {
        return Err(GatewayError::with_reason(
            reasons::UNSUPPORTED_CONTENT_TYPE,
            Stage::Parse,
            Message::Fixed("The request Content-Type must be multipart/form-data with a boundary."),
        ));
    };
    cx.log.stage(Stage::Parse);
    let head = crate::upload::read(
        cx,
        parts,
        incoming,
        &boundary,
        &crate::upload::Wanted { files: &["file"] },
    )
    .await?;
    if head.rest.is_none() && !head.scanner.closed() {
        return Err(crate::upload::malformed());
    }
    let form = head.scanner.form();
    let present = |n: &str| form.parts().iter().any(|p| p.name.as_deref() == Some(n));
    let file_seen =
        present("file") || head.scanner.open_part().and_then(|p| p.name).as_deref() == Some("file");
    for (name, seen) in [("file", file_seen), ("purpose", present("purpose"))] {
        if !seen {
            return Err(GatewayError::with_reason(
                reasons::MISSING_REQUIRED_PARAMETER,
                Stage::Parse,
                Message::built(format!("{name}: a {name} part is required.")),
            )
            .param(name)
            .openai_code("missing_required_parameter"));
        }
    }
    cx.log.stage(Stage::Resolve);
    let provider = choose_provider(cx, parts, &key)?;
    let Some(carrier) = provider_call::carrier(cx, &key, &provider, "files") else {
        return Err(no_allowed_provider(&provider));
    };
    let end_user = auth.end_user.clone();
    crate::access::require_end_user(auth, end_user.as_deref())?;
    let tags = tags(cx, auth);
    crate::access::check(
        cx,
        &crate::access::Subject {
            parts,
            auth,
            end_user: end_user.as_deref(),
            requested: "",
            model: "",
            tags: &tags,
        },
    )
    .await?;
    let upload = head.rest.clone();
    let parsed = Parsed::Multipart {
        raw: head.raw.clone(),
        form: form.clone(),
        model: antirion_dialects::ModelField::Missing,
        stream: false,
        rest: head.rest,
    };
    cx.log.stage(Stage::Admit);
    let candidates = [&carrier];
    account::admit(
        cx,
        &cx.account,
        &Admission {
            endpoint,
            parsed: &parsed,
            key: &key,
            end_user: end_user.as_deref(),
            model: "",
            route: None,
            tags: &tags,
            candidates: &candidates,
            stream: false,
            hedge: 1,
        },
    )
    .await?;
    let mut body = Payload::from(head.raw);
    if let Some(u) = &upload {
        body = body.with_tail(u.tail());
    }
    let call = Call {
        method: Method::POST,
        class: "files",
        path: "/files".to_owned(),
        body,
        content_type,
        kind: RequestKind::SideEffecting,
        whole: true,
        upload,
    };
    cx.log.stage(Stage::Route);
    let slot = cx.account.clone();
    let sent = provider_call::send(cx, parts, endpoint, &key, carrier, &call).await;
    let resp = match sent {
        Ok(answer) => keep_upload(cx, &key, &provider, answer).await,
        Err(e) => Err(e),
    };
    let end = match &resp {
        Ok(r) => End::Response(r.status().as_u16()),
        Err(_) => End::Failed,
    };
    crate::account::no_tokens(&cx.log);
    account::settle(&slot, &cx.log, end).await;
    let mut resp = match resp {
        Ok(r) => r,
        Err(e) => cx.error(&e),
    };
    slot.decorate(&mut resp, false);
    Ok(resp)
}

/// Records an uploaded file's mapping and answers with the gateway id.
async fn keep_upload(
    cx: &Ctx,
    key: &KeyRt,
    provider: &str,
    answer: Answered,
) -> Result<Response<Body>, GatewayError> {
    let AnswerBody::Whole(raw) = answer.body else {
        return Err(bad_answer());
    };
    let v: Value = serde_json::from_slice(&raw).map_err(|_| bad_answer())?;
    let Some(upstream_id) = v.get("id").and_then(Value::as_str).map(str::to_owned) else {
        return Err(bad_answer());
    };
    let id = new_id(FILE_PREFIX);
    let obj = UpstreamObject {
        kind: "file".into(),
        id: id.clone(),
        project: key.project.clone(),
        provider: provider.to_owned(),
        upstream_id,
        created_us: cx.rt.now().0,
        body: v.to_string(),
    };
    cx.rt
        .store
        .put_object(obj)
        .await
        .map_err(|_| store_unavailable())?;
    Ok(json_response(cx, answer.status, &shown(v, &id)))
}

pub(crate) fn bad_answer() -> GatewayError {
    GatewayError::with_reason(
        antirion_error::reason::hygiene::INVALID_BODY,
        Stage::Attempt,
        Message::Fixed("The upstream answer is not the object the gateway expected."),
    )
}

/// `GET /v1/files`: the project's files, newest first, from the gateway's mappings.
async fn list(cx: &Ctx, parts: &Parts, key: &KeyRt) -> Result<Response<Body>, GatewayError> {
    let query = parts.uri.query().unwrap_or_default();
    let param = |name: &str| {
        query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (k == name).then(|| v.to_owned())
        })
    };
    let limit = param("limit")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(10_000)
        .clamp(1, 10_000);
    let before = match param("after") {
        Some(after) => match owned(cx, "file", &after, &key.project).await? {
            Some(o) => Some(o.created_us),
            None => return Err(file_not_found().param("after")),
        },
        None => None,
    };
    let purpose = param("purpose");
    let rows = cx
        .rt
        .store
        .objects("file", &key.project, before, limit + 1)
        .await
        .map_err(|_| store_unavailable())?;
    let mut data: Vec<Value> = rows
        .iter()
        .filter_map(|o| {
            let v: Value = serde_json::from_str(&o.body).ok()?;
            let keep = purpose
                .as_deref()
                .is_none_or(|p| v.get("purpose").and_then(Value::as_str) == Some(p));
            keep.then(|| shown(v, &o.id))
        })
        .collect();
    let has_more = data.len() as u64 > limit;
    data.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let first = data.first().and_then(|v| v.get("id")).cloned();
    let last = data.last().and_then(|v| v.get("id")).cloned();
    Ok(json_response(
        cx,
        StatusCode::OK,
        &json!({"object": "list", "data": data, "first_id": first, "last_id": last, "has_more": has_more}),
    ))
}

/// Retrieve, delete or download one file through its provider.
async fn object(
    cx: &Ctx,
    parts: &Parts,
    endpoint: &Endpoint,
    key: &Arc<KeyRt>,
    method: &Method,
    id: &str,
    action: Option<&str>,
) -> Result<Response<Body>, GatewayError> {
    let Some(obj) = owned(cx, "file", id, &key.project).await? else {
        return Err(file_not_found());
    };
    let Some(carrier) = provider_call::carrier(cx, key, &obj.provider, "files") else {
        return Err(crate::respstate::affinity_unavailable());
    };
    let encoded = antirion_providers::sigv4::uri_encode(&obj.upstream_id);
    let (path, kind, whole) = match (method, action) {
        (&Method::GET, Some("content")) => (
            format!("/files/{encoded}/content"),
            RequestKind::ResourceBound,
            false,
        ),
        (&Method::DELETE, None) => (format!("/files/{encoded}"), RequestKind::SideEffecting, true),
        _ => (format!("/files/{encoded}"), RequestKind::ResourceBound, true),
    };
    let call = Call {
        method: method.clone(),
        class: "files",
        path,
        body: Payload::default(),
        content_type: None,
        kind,
        whole,
        upload: None,
    };
    let answer = provider_call::send(cx, parts, endpoint, key, carrier, &call).await?;
    match answer.body {
        AnswerBody::Streamed(body) => {
            let mut resp = Response::new(streamed(crate::outcome::data_stream(body)));
            *resp.status_mut() = answer.status;
            if let Some(ct) = answer.headers.get(CONTENT_TYPE).cloned() {
                resp.headers_mut().insert(CONTENT_TYPE, ct);
            }
            cx.finish(&mut resp, false);
            Ok(resp)
        }
        AnswerBody::Whole(raw) => {
            let v: Value = serde_json::from_slice(&raw).map_err(|_| bad_answer())?;
            if method == Method::DELETE {
                cx.rt
                    .store
                    .delete_object("file", id)
                    .await
                    .map_err(|_| store_unavailable())?;
            } else {
                let mut kept = obj.clone();
                kept.body = v.to_string();
                cx.rt
                    .store
                    .put_object(kept)
                    .await
                    .map_err(|_| store_unavailable())?;
            }
            Ok(json_response(cx, answer.status, &shown(v, id)))
        }
    }
}

/// The gateway file ids an inference body carries in `file_id` members.
fn file_ids(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                match x {
                    Value::String(s) if k == "file_id" && s.starts_with(FILE_PREFIX) => out.push(s.clone()),
                    _ => file_ids(x, out),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| file_ids(x, out)),
        _ => {}
    }
}

fn replace_ids(v: &mut Value, map: &[(String, String)]) {
    match v {
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                if k == "file_id"
                    && let Value::String(s) = x
                    && let Some((_, up)) = map.iter().find(|(g, _)| g == s)
                {
                    *x = Value::from(up.as_str());
                } else {
                    replace_ids(x, map);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| replace_ids(x, map)),
        _ => {}
    }
}

/// Replaces the gateway file ids of an inference body with the provider's and pins the request
/// to that provider. A body that names files of two providers cannot be served.
pub(crate) async fn resolve_in_body(cx: &Ctx, key: &KeyRt, parsed: Parsed) -> Result<Parsed, GatewayError> {
    let Parsed::Json { raw, .. } = &parsed else {
        return Ok(parsed);
    };
    if !raw
        .windows(FILE_PREFIX.len())
        .any(|w| w == FILE_PREFIX.as_bytes())
    {
        return Ok(parsed);
    }
    let Some(doc) = parsed.doc() else {
        return Ok(parsed);
    };
    let mut ids = Vec::new();
    file_ids(doc, &mut ids);
    if ids.is_empty() {
        return Ok(parsed);
    }
    let mut map = Vec::with_capacity(ids.len());
    let mut provider: Option<String> = None;
    for id in ids {
        let Some(obj) = owned(cx, "file", &id, &key.project).await? else {
            return Err(file_not_found());
        };
        match &provider {
            Some(p) if *p != obj.provider => {
                return Err(GatewayError::with_reason(
                    reasons::INVALID_VALUE,
                    Stage::Resolve,
                    Message::Fixed("The request names files stored with different providers."),
                )
                .param("file_id")
                .openai_code("invalid_value"));
            }
            _ => provider = Some(obj.provider.clone()),
        }
        map.push((id, obj.upstream_id));
    }
    let mut v = doc.clone();
    replace_ids(&mut v, &map);
    let raw = Bytes::from(serde_json::to_vec(&v).unwrap_or_default());
    let Ok(skim) = antirion_json::skim(&raw, &antirion_json::JsonLimits::default()) else {
        return Ok(parsed);
    };
    let mut out = Parsed::json(raw, skim, Some(v));
    out.pin(provider);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_their_shape() {
        let id = new_id(FILE_PREFIX);
        assert!(gateway_id(&id, FILE_PREFIX), "{id}");
        assert_ne!(new_id(FILE_PREFIX), id);
        assert!(!gateway_id("file-ant_short", FILE_PREFIX));
        assert!(gateway_id(
            &format!("{FILE_PREFIX}{}", "a".repeat(26)),
            FILE_PREFIX
        ));
    }

    #[test]
    fn file_ids_are_found_and_replaced() {
        let g = format!("{FILE_PREFIX}{}", "b".repeat(26));
        let mut v = json!({"messages": [{"content": [{"type": "file", "file": {"file_id": g}}]}],
                           "input": [{"type": "input_file", "file_id": "file-other"}]});
        let mut ids = Vec::new();
        file_ids(&v, &mut ids);
        assert_eq!(ids, std::slice::from_ref(&g));
        replace_ids(&mut v, &[(g, "file-up".into())]);
        assert_eq!(v["messages"][0]["content"][0]["file"]["file_id"], "file-up");
        assert_eq!(v["input"][0]["file_id"], "file-other");
    }
}

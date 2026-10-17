//! The Batches API. A batch goes to the provider of its input file. Before it is submitted, the
//! file is read from the provider line by line, each line's `model` rewritten to the upstream id of
//! the deployment of that provider that serves it, and the result uploaded as the batch's input
//! (a line no such deployment serves refuses the batch, naming the line). Batch ids are mapped like
//! file ids. The batch's usage is settled once, the first time it is seen completed: its output
//! file is read and each deployment's usage written as a ledger line of the request that created it.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use antirion_core::Stage;
use antirion_dialects::Endpoint;
use antirion_egress::{InboundBody, Payload, Tail, TailError};
use antirion_error::{GatewayError, Message};
use antirion_providers::DeploymentRt;
use antirion_resilience::RequestKind;
use antirion_runtime::Snapshot;
use antirion_runtime::identity::KeyRt;
use antirion_store::UpstreamObject;
use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt};
use http::request::Parts;
use http::{HeaderValue, Method, Response, StatusCode};
use http_body::Body as _;
use hyper::body::Incoming;
use serde_json::{Map, Value, json};

use crate::account::{self, Admission, Deferred, DeferredCall, End};
use crate::auth::Authenticated;
use crate::ctx::{Body, Ctx};
use crate::files::{self, bad_answer, json_response, store_unavailable};
use crate::provider_call::{self, AnswerBody, Call};
use crate::reasons;

/// The prefix of a gateway batch id.
pub(crate) const BATCH_PREFIX: &str = "batch-ant_";

/// The ledger line numbers of a batch's usage start after the line of the call that created it.
const FIRST_USAGE_LINE: u32 = 100;

fn batch_not_found() -> GatewayError {
    GatewayError::with_reason(
        reasons::BATCH_NOT_FOUND,
        Stage::Resolve,
        Message::Fixed("No such batch."),
    )
}

/// Serves a Batches endpoint.
pub(crate) async fn serve(
    cx: &mut Ctx,
    parts: &Parts,
    incoming: Incoming,
    endpoint: &Endpoint,
    auth: &Authenticated,
) -> Response<Body> {
    let id = endpoint.path_param.clone();
    files::uncacheable(cx, &auth.key);
    let result = match (&parts.method, id.as_deref(), endpoint.action) {
        (&Method::POST, None, None) => create(cx, parts, incoming, endpoint, auth).await,
        (&Method::GET, None, None) => list(cx, parts, &auth.key).await,
        (m, Some(id), action) => one(cx, parts, endpoint, &auth.key, m, id, action).await,
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

fn required_string<'a>(doc: &'a Value, name: &str) -> Result<&'a str, GatewayError> {
    match doc.get(name) {
        None | Some(Value::Null) => Err(GatewayError::with_reason(
            reasons::MISSING_REQUIRED_PARAMETER,
            Stage::Parse,
            Message::built(format!("{name}: a value is required.")),
        )
        .param(name)
        .openai_code("missing_required_parameter")),
        Some(Value::String(s)) if !s.is_empty() => Ok(s),
        Some(_) => Err(GatewayError::with_reason(
            reasons::INVALID_TYPE,
            Stage::Parse,
            Message::built(format!("{name} must be a non-empty string.")),
        )
        .param(name)
        .openai_code("invalid_type")),
    }
}

/// The deployment of `provider` that serves `model` for the key: the first, in the Model's order,
/// the key may use.
fn deployment_for(snap: &Snapshot, key: &KeyRt, provider: &str, model: &str) -> Option<Arc<DeploymentRt>> {
    let target = crate::authorize::resolve(snap, key, model).ok()?;
    let resolved = crate::infer::model::resolution(snap, model, &target).ok()?;
    let levels = snap.identity.access_levels(key);
    resolved.known.into_iter().find(|d| {
        d.provider.name == provider && d.usable() && crate::authorize::deployment_allowed(&levels, d)
    })
}

async fn create(
    cx: &mut Ctx,
    parts: &Parts,
    incoming: Incoming,
    endpoint: &Endpoint,
    auth: &Authenticated,
) -> Result<Response<Body>, GatewayError> {
    let key = Arc::clone(&auth.key);
    cx.log.stage(Stage::Parse);
    let parsed = crate::parse::run(cx, parts, incoming, endpoint).await?;
    let doc = parsed.doc().cloned().unwrap_or(Value::Null);
    let input = required_string(&doc, "input_file_id")?.to_owned();
    required_string(&doc, "endpoint")?;
    required_string(&doc, "completion_window")?;
    cx.log.stage(Stage::Resolve);
    let Some(file) = files::owned(cx, "file", &input, &key.project).await? else {
        return Err(files::file_not_found().param("input_file_id"));
    };
    let Some(carrier) = provider_call::carrier(cx, &key, &file.provider, "batches") else {
        return Err(crate::respstate::affinity_unavailable());
    };
    let end_user = auth.end_user.clone();
    crate::access::require_end_user(auth, end_user.as_deref())?;
    let tags = files::tags(cx, auth);
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
    cx.log.stage(Stage::Route);
    let slot = cx.account.clone();
    let deferred = slot.deferred();
    let result = submit(cx, parts, endpoint, &key, &carrier, &file, &doc, &input, deferred).await;
    let end = match &result {
        Ok(r) => End::Response(r.status().as_u16()),
        Err(_) => End::Failed,
    };
    crate::account::no_tokens(&cx.log);
    account::settle(&slot, &cx.log, end).await;
    let mut resp = result.unwrap_or_else(|e| cx.error(&e));
    slot.decorate(&mut resp, false);
    Ok(resp)
}

#[allow(clippy::too_many_arguments)]
async fn submit(
    cx: &Ctx,
    parts: &Parts,
    endpoint: &Endpoint,
    key: &Arc<KeyRt>,
    carrier: &Arc<DeploymentRt>,
    file: &UpstreamObject,
    doc: &Value,
    input: &str,
    deferred: Option<Deferred>,
) -> Result<Response<Body>, GatewayError> {
    let encoded = antirion_providers::sigv4::uri_encode(&file.upstream_id);
    let download = Call {
        method: Method::GET,
        class: "files",
        path: format!("/files/{encoded}/content"),
        body: Payload::default(),
        content_type: None,
        kind: RequestKind::ResourceBound,
        whole: false,
        upload: None,
    };
    let content = provider_call::send(cx, parts, endpoint, key, Arc::clone(carrier), &download).await?;
    let AnswerBody::Streamed(body) = content.body else {
        return Err(bad_answer());
    };
    let boundary = format!("antirion-batch-{:032x}", antirion_core::ResourceUid::random().0);
    let head = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"purpose\"\r\n\r\nbatch\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"batch.jsonl\"\r\nContent-Type: application/jsonl\r\n\r\n"
    );
    let state = Arc::new(Mutex::new(Rewrite::default()));
    let lines = Lines {
        body,
        buf: BytesMut::new(),
        line: 0,
        done: false,
        closing: Some(Bytes::from(format!("\r\n--{boundary}--\r\n"))),
        snap: Arc::clone(&cx.snap),
        key: Arc::clone(key),
        provider: file.provider.clone(),
        state: Arc::clone(&state),
    };
    let content_type = HeaderValue::from_str(&format!("multipart/form-data; boundary={boundary}")).ok();
    let upload = Call {
        method: Method::POST,
        class: "files",
        path: "/files".to_owned(),
        body: Payload::from(Bytes::from(head)).with_tail(Tail::new(Box::pin(lines), None)),
        content_type,
        kind: RequestKind::SideEffecting,
        whole: true,
        upload: None,
    };
    let uploaded = provider_call::send(cx, parts, endpoint, key, Arc::clone(carrier), &upload).await;
    let refused = state.lock().ok().and_then(|mut s| s.error.take());
    if let Some(e) = refused {
        return Err(e);
    }
    let uploaded = uploaded?;
    let AnswerBody::Whole(raw) = uploaded.body else {
        return Err(bad_answer());
    };
    let uploaded: Value = serde_json::from_slice(&raw).map_err(|_| bad_answer())?;
    let Some(rewritten) = uploaded.get("id").and_then(Value::as_str) else {
        return Err(bad_answer());
    };
    let mut request: Map<String, Value> = doc.as_object().cloned().unwrap_or_default();
    request.insert("input_file_id".into(), Value::from(rewritten));
    let create = Call {
        method: Method::POST,
        class: "batches",
        path: "/batches".to_owned(),
        body: Payload::from(serde_json::to_vec(&request).unwrap_or_default()),
        content_type: Some(HeaderValue::from_static("application/json")),
        kind: RequestKind::SideEffecting,
        whole: true,
        upload: None,
    };
    let created = provider_call::send(cx, parts, endpoint, key, Arc::clone(carrier), &create).await?;
    let AnswerBody::Whole(raw) = created.body else {
        return Err(bad_answer());
    };
    let batch: Value = serde_json::from_slice(&raw).map_err(|_| bad_answer())?;
    let Some(upstream_id) = batch.get("id").and_then(Value::as_str).map(str::to_owned) else {
        return Err(bad_answer());
    };
    let deployments: Map<String, Value> = state
        .lock()
        .map(|s| {
            s.deployments
                .iter()
                .map(|(m, d)| (m.clone(), Value::from(d.as_str())))
                .collect()
        })
        .unwrap_or_default();
    let id = files::new_id(BATCH_PREFIX);
    let kept = json!({
        "object": batch,
        "input_file_id": input,
        "deployments": deployments,
        "deferred": deferred.map(|d| d.to_json()),
        "settled": false,
    });
    let obj = UpstreamObject {
        kind: "batch".into(),
        id: id.clone(),
        project: key.project.clone(),
        provider: file.provider.clone(),
        upstream_id,
        created_us: cx.rt.now().0,
        body: kept.to_string(),
    };
    cx.rt
        .store
        .put_object(obj.clone())
        .await
        .map_err(|_| store_unavailable())?;
    let shown = shown(cx, &obj, &kept).await?;
    Ok(json_response(cx, created.status, &shown))
}

/// What the rewrite of a batch's input saw: the deployment of each model, or why it refused.
#[derive(Default)]
struct Rewrite {
    deployments: HashMap<String, String>,
    error: Option<GatewayError>,
}

/// The input file's lines with their models rewritten, followed by the form's closing delimiter.
struct Lines {
    body: InboundBody,
    buf: BytesMut,
    line: u64,
    done: bool,
    closing: Option<Bytes>,
    snap: Arc<Snapshot>,
    key: Arc<KeyRt>,
    provider: String,
    state: Arc<Mutex<Rewrite>>,
}

impl Lines {
    fn refuse(&self, e: GatewayError) -> Poll<Option<Result<Bytes, TailError>>> {
        if let Ok(mut s) = self.state.lock() {
            s.error = Some(e);
        }
        Poll::Ready(Some(Err(Box::new(BatchRefused))))
    }

    /// One input line rewritten, with its newline.
    fn rewrite(&mut self, raw: &[u8]) -> Result<Option<Bytes>, GatewayError> {
        self.line += 1;
        let text = raw.strip_suffix(b"\r").unwrap_or(raw);
        if text.iter().all(u8::is_ascii_whitespace) {
            return Ok(None);
        }
        let mismatch = |line: u64, what: String| {
            GatewayError::with_reason(
                reasons::BATCH_MODEL_MISMATCH,
                Stage::Resolve,
                Message::built(format!("Line {line} of the input file {what}.")),
            )
            .param("input_file_id")
        };
        let mut v: Value =
            serde_json::from_slice(text).map_err(|_| mismatch(self.line, "is not valid JSON".into()))?;
        if let Some(url) = v.get("url").and_then(Value::as_str) {
            let method = v
                .get("method")
                .and_then(Value::as_str)
                .and_then(|m| m.parse::<Method>().ok())
                .unwrap_or(Method::POST);
            if let antirion_dialects::endpoints::Matched::Found(e) =
                antirion_dialects::endpoints::match_endpoint(&method, url)
                && !crate::auth::family_allowed(&self.snap.identity, &self.key, e.family)
            {
                return Err(GatewayError::with_reason(
                    antirion_identity::reasons::ENDPOINT_NOT_ALLOWED,
                    Stage::Authenticate,
                    Message::built(format!(
                        "Line {} of the input file calls {url}, which the API key may not use.",
                        self.line
                    )),
                )
                .param("input_file_id"));
            }
        }
        let model = v
            .pointer("/body/model")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| mismatch(self.line, "names no model".into()))?;
        let d = deployment_for(&self.snap, &self.key, &self.provider, &model).ok_or_else(|| {
            mismatch(
                self.line,
                format!(
                    "names model `{model}`, which no deployment of provider `{}` serves",
                    self.provider
                ),
            )
        })?;
        if let Ok(mut s) = self.state.lock() {
            s.deployments.insert(d.model.clone(), d.name.clone());
        }
        if let Some(m) = v.pointer_mut("/body/model") {
            *m = Value::from(d.model.as_str());
        }
        let mut out = serde_json::to_vec(&v).unwrap_or_default();
        out.push(b'\n');
        Ok(Some(Bytes::from(out)))
    }
}

/// The batch's input was refused while it was being uploaded.
#[derive(Debug, thiserror::Error)]
#[error("the batch input was refused")]
struct BatchRefused;

impl Stream for Lines {
    type Item = Result<Bytes, TailError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(nl) = self.buf.iter().position(|b| *b == b'\n') {
                let line = self.buf.split_to(nl + 1);
                match self.rewrite(line.get(..nl).unwrap_or_default()) {
                    Ok(Some(out)) => return Poll::Ready(Some(Ok(out))),
                    Ok(None) => continue,
                    Err(e) => return self.refuse(e),
                }
            }
            if self.done {
                if !self.buf.is_empty() {
                    let rest = self.buf.split();
                    match self.rewrite(&rest) {
                        Ok(Some(out)) => return Poll::Ready(Some(Ok(out))),
                        Ok(None) => continue,
                        Err(e) => return self.refuse(e),
                    }
                }
                return Poll::Ready(self.closing.take().map(Ok));
            }
            match Pin::new(&mut self.body).poll_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => self.done = true,
                Poll::Ready(Some(Err(_))) => {
                    return self.refuse(bad_answer());
                }
                Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        self.buf.extend_from_slice(&data);
                    }
                }
            }
        }
    }
}

/// The client's view of a batch: the gateway's ids for the batch and its files.
async fn shown(cx: &Ctx, obj: &UpstreamObject, kept: &Value) -> Result<Value, GatewayError> {
    let mut v = kept.get("object").cloned().unwrap_or(Value::Null);
    let input = kept.get("input_file_id").cloned().unwrap_or(Value::Null);
    let mut mapped = Vec::new();
    for field in ["output_file_id", "error_file_id"] {
        if let Some(up) = v.get(field).and_then(Value::as_str) {
            let g = files::map_upstream_file(cx, &obj.project, &obj.provider, up).await?;
            mapped.push((field, g));
        }
    }
    if let Some(o) = v.as_object_mut() {
        o.insert("id".into(), Value::from(obj.id.as_str()));
        o.insert("input_file_id".into(), input);
        for (field, g) in mapped {
            o.insert(field.into(), Value::from(g));
        }
    }
    Ok(v)
}

/// `GET /v1/batches`: the project's batches as last seen, newest first.
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
        .unwrap_or(20)
        .clamp(1, 100);
    let before = match param("after") {
        Some(after) => match files::owned(cx, "batch", &after, &key.project).await? {
            Some(o) => Some(o.created_us),
            None => return Err(batch_not_found().param("after")),
        },
        None => None,
    };
    let rows = cx
        .rt
        .store
        .objects("batch", &key.project, before, limit + 1)
        .await
        .map_err(|_| store_unavailable())?;
    let has_more = rows.len() as u64 > limit;
    let mut data = Vec::new();
    for o in rows.iter().take(usize::try_from(limit).unwrap_or(usize::MAX)) {
        let kept: Value = serde_json::from_str(&o.body).unwrap_or(Value::Null);
        data.push(shown(cx, o, &kept).await?);
    }
    let first = data.first().and_then(|v| v.get("id")).cloned();
    let last = data.last().and_then(|v| v.get("id")).cloned();
    Ok(json_response(
        cx,
        StatusCode::OK,
        &json!({"object": "list", "data": data, "first_id": first, "last_id": last, "has_more": has_more}),
    ))
}

/// Retrieves or cancels one batch through its provider.
async fn one(
    cx: &Ctx,
    parts: &Parts,
    endpoint: &Endpoint,
    key: &Arc<KeyRt>,
    method: &Method,
    id: &str,
    action: Option<&str>,
) -> Result<Response<Body>, GatewayError> {
    let Some(obj) = files::owned(cx, "batch", id, &key.project).await? else {
        return Err(batch_not_found());
    };
    let Some(carrier) = provider_call::carrier(cx, key, &obj.provider, "batches") else {
        return Err(crate::respstate::affinity_unavailable());
    };
    let encoded = antirion_providers::sigv4::uri_encode(&obj.upstream_id);
    let (path, kind) = match (method, action) {
        (&Method::POST, Some("cancel")) => (format!("/batches/{encoded}/cancel"), RequestKind::SideEffecting),
        _ => (format!("/batches/{encoded}"), RequestKind::ResourceBound),
    };
    let call = Call {
        method: method.clone(),
        class: "batches",
        path,
        body: Payload::default(),
        content_type: None,
        kind,
        whole: true,
        upload: None,
    };
    let answer = provider_call::send(cx, parts, endpoint, key, Arc::clone(&carrier), &call).await?;
    let AnswerBody::Whole(raw) = answer.body else {
        return Err(bad_answer());
    };
    let batch: Value = serde_json::from_slice(&raw).map_err(|_| bad_answer())?;
    let mut kept: Value = serde_json::from_str(&obj.body).unwrap_or_else(|_| json!({}));
    if let Some(o) = kept.as_object_mut() {
        o.insert("object".into(), batch.clone());
    }
    let mut updated = obj.clone();
    updated.body = kept.to_string();
    let swapped = cx
        .rt
        .store
        .swap_object("batch", id, &obj.body, &updated.body)
        .await
        .map_err(|_| store_unavailable())?;
    if swapped {
        settle_if_done(cx, parts, endpoint, key, &carrier, &updated, &kept).await;
    }
    let shown = shown(cx, &obj, &kept).await?;
    Ok(json_response(cx, answer.status, &shown))
}

/// Whether a batch status is final.
fn finished(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "expired" | "cancelled")
}

/// Settles a finished batch's usage once: marks it settled (one node wins), reads its output file
/// and writes a ledger line per deployment.
async fn settle_if_done(
    cx: &Ctx,
    parts: &Parts,
    endpoint: &Endpoint,
    key: &Arc<KeyRt>,
    carrier: &Arc<DeploymentRt>,
    obj: &UpstreamObject,
    kept: &Value,
) {
    let status = kept
        .pointer("/object/status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !finished(status) || kept.get("settled") != Some(&Value::Bool(false)) {
        return;
    }
    let Some(deferred) = kept
        .get("deferred")
        .and_then(|d| Deferred::from_json(d, "batches"))
    else {
        return;
    };
    let mut done = kept.clone();
    if let Some(o) = done.as_object_mut() {
        o.insert("settled".into(), Value::Bool(true));
    }
    let done = done.to_string();
    if !matches!(
        cx.rt.store.swap_object("batch", &obj.id, &obj.body, &done).await,
        Ok(true)
    ) {
        return;
    }
    let Some(output) = kept.pointer("/object/output_file_id").and_then(Value::as_str) else {
        return;
    };
    let call = Call {
        method: Method::GET,
        class: "files",
        path: format!("/files/{}/content", antirion_providers::sigv4::uri_encode(output)),
        body: Payload::default(),
        content_type: None,
        kind: RequestKind::ResourceBound,
        whole: false,
        upload: None,
    };
    let Ok(answer) = provider_call::send(cx, parts, endpoint, key, Arc::clone(carrier), &call).await else {
        return;
    };
    let AnswerBody::Streamed(body) = answer.body else {
        return;
    };
    let deployments: HashMap<String, String> = kept
        .get("deployments")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|d| (k.clone(), d.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    let sums = usage_by_deployment(body, &deployments).await;
    let mut names: Vec<&String> = sums.keys().collect();
    names.sort();
    for (i, name) in names.into_iter().enumerate() {
        let Some(d) = cx.snap.providers.deployments.get(name) else {
            continue;
        };
        let Some(usage) = sums.get(name) else {
            continue;
        };
        let _ = account::settle_deferred(
            &cx.rt,
            &cx.snap,
            &deferred,
            DeferredCall {
                deployment: d,
                line: FIRST_USAGE_LINE + u32::try_from(i).unwrap_or(0),
                usage: *usage,
                raw: None,
                batch: true,
                purpose: antirion_limits::ledger::Purpose::Primary,
                status: antirion_limits::ledger::LineStatus::Success,
            },
        )
        .await;
    }
}

/// The deployment a result line's reported model belongs to: an exact upstream id, else one the
/// reported (dated) name starts with, else the batch's only deployment.
fn owner_of<'a>(model: &str, deployments: &'a HashMap<String, String>) -> Option<&'a String> {
    if let Some(d) = deployments.get(model) {
        return Some(d);
    }
    if let Some((_, d)) = deployments
        .iter()
        .filter(|(m, _)| model.starts_with(m.as_str()))
        .max_by_key(|(m, _)| m.len())
    {
        return Some(d);
    }
    match deployments.values().collect::<Vec<_>>().as_slice() {
        [one] => Some(one),
        _ => None,
    }
}

/// Sums the usage of a batch's output lines by deployment.
async fn usage_by_deployment(
    body: InboundBody,
    deployments: &HashMap<String, String>,
) -> HashMap<String, antirion_limits::usage::Usage> {
    let mut sums: HashMap<String, antirion_limits::usage::Usage> = HashMap::new();
    let mut frames = crate::outcome::data_stream(body);
    let mut buf = BytesMut::new();
    let mut add = |line: &[u8]| {
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        let Some(body) = v.pointer("/response/body") else {
            return;
        };
        let model = body.get("model").and_then(Value::as_str).unwrap_or_default();
        let Some(owner) = owner_of(model, deployments) else {
            return;
        };
        let usage = crate::apis::usage_of(body).normalised();
        let entry = sums.entry(owner.clone()).or_default();
        *entry = entry.plus(&usage);
    };
    loop {
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line = buf.split_to(nl + 1);
            add(line.get(..nl).unwrap_or_default());
        }
        match frames.next().await {
            Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
            Some(Err(_)) => break,
            None => {
                add(&buf);
                break;
            }
        }
    }
    sums
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_lines_find_their_deployment() {
        let mut d = HashMap::new();
        d.insert("gpt-x".to_owned(), "dep-x".to_owned());
        assert_eq!(owner_of("gpt-x", &d).map(String::as_str), Some("dep-x"));
        assert_eq!(
            owner_of("gpt-x-2025-01-01", &d).map(String::as_str),
            Some("dep-x")
        );
        assert_eq!(owner_of("other", &d).map(String::as_str), Some("dep-x"));
        d.insert("gpt-y".to_owned(), "dep-y".to_owned());
        assert_eq!(owner_of("other", &d), None);
        assert!(finished("completed") && !finished("in_progress"));
    }
}

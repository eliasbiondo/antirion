//! Provider actions: `:test` checks a credential with the cheapest call the provider's type has,
//! and `:rotate_credential` puts a new credential in place, keeping the old one as a standby until
//! its overlap ends.

use std::time::{Duration, Instant};

use antirion_core::audit::Outcome;
use antirion_core::{RequestId, UtcMicros};
use antirion_egress::Payload;
use antirion_identity::rbac::Scope;
use antirion_limits::cost::CostStatus;
use antirion_limits::estimate::OutSource;
use antirion_limits::ledger::{CallTarget, LedgerLine, LineStatus, Purpose, Reserved, UsageSource};
use antirion_limits::money::{Dec, Nusd};
use antirion_limits::usage::{Units, Usage};
use antirion_providers::{DeploymentRt, ProviderRt, ProviderType, UpstreamCall, UpstreamOp};
use antirion_runtime::changes::Target;
use axum::response::Response;
use http::{HeaderValue, Method, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::commit::{Proposal, api_write};
use crate::error::AdminError;

/// The longest a test waits for its answer.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);
/// The bytes of a test's answer read.
const TEST_BODY: usize = 64 << 10;
/// The longest overlap a rotation keeps the old credential.
const MAX_OVERLAP: Duration = Duration::from_secs(7 * 86_400);

fn invalid(field: &str, code: antirion_config::DetailCode, message: &str) -> AdminError {
    AdminError::validation(vec![antirion_config::ConfigError::new(
        &antirion_config::DocPath::root().key(field),
        code,
        message,
    )])
}

fn members(call: &Call, allowed: &[&str]) -> Result<Map<String, Value>, AdminError> {
    let body = call.json_body()?;
    if let Some(k) = body.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(invalid(
            k,
            antirion_config::DetailCode::UnknownField,
            "unknown member",
        ));
    }
    Ok(body)
}

fn provider<'a>(call: &'a Call, name: &str) -> Result<&'a std::sync::Arc<ProviderRt>, AdminError> {
    call.snap
        .providers
        .providers
        .get(name)
        .ok_or_else(AdminError::not_found)
}

/// What a test sends: a models list where the type has one, else a one-token completion.
fn probe(d: &DeploymentRt) -> (UpstreamOp<'static>, Option<Value>) {
    let ping = "ping";
    match d.provider.kind {
        k if k.openai_family() && k != ProviderType::AzureOpenAi => (
            UpstreamOp::OpenAi {
                class: "models",
                path: "/models",
            },
            None,
        ),
        k if k.anthropic_api() => (
            UpstreamOp::Messages,
            Some(json!({"model": d.model, "max_tokens": 1, "messages": [{"role": "user", "content": ping}]})),
        ),
        ProviderType::Gemini | ProviderType::Vertex => (
            UpstreamOp::GeminiGenerate,
            Some(json!({"contents": [{"role": "user", "parts": [{"text": ping}]}],
                        "generationConfig": {"maxOutputTokens": 1}})),
        ),
        ProviderType::Bedrock => (
            UpstreamOp::Converse,
            Some(
                json!({"messages": [{"role": "user", "content": [{"text": ping}]}],
                        "inferenceConfig": {"maxTokens": 1}}),
            ),
        ),
        ProviderType::Cohere => (
            UpstreamOp::CohereChat,
            Some(json!({"model": d.model, "max_tokens": 1, "messages": [{"role": "user", "content": ping}]})),
        ),
        _ => (
            UpstreamOp::OpenAi {
                class: "chat",
                path: "/chat/completions",
            },
            Some(json!({"model": d.model, "max_tokens": 1, "messages": [{"role": "user", "content": ping}]})),
        ),
    }
}

/// The failure class of a test's answer.
fn class_of(status: u16) -> &'static str {
    match status {
        401 | 403 => "auth",
        404 => "not_found",
        408 => "timeout",
        429 => "rate_limited",
        500..=599 => "server_error",
        _ => "caller_error",
    }
}

/// An upstream error's message, with every secret of the provider taken out.
fn scrubbed(body: &[u8], secrets: &[String]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let m = v
        .pointer("/error/message")
        .or_else(|| v.get("message"))
        .or_else(|| v.pointer("/error"))
        .and_then(Value::as_str)?;
    let mut out: String = m.chars().take(500).collect();
    for s in secrets.iter().filter(|s| s.len() >= 8) {
        out = out.replace(s.as_str(), "[REDACTED]");
    }
    Some(out)
}

/// The tokens a one-token completion reports, in any provider's shape.
fn usage_of(body: &[u8]) -> Usage {
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return Usage::default();
    };
    let n = |paths: &[&str]| {
        paths
            .iter()
            .find_map(|p| v.pointer(p).and_then(Value::as_u64))
            .unwrap_or(0)
    };
    Usage::estimated(
        n(&[
            "/usage/prompt_tokens",
            "/usage/input_tokens",
            "/usage/inputTokens",
            "/usageMetadata/promptTokenCount",
        ]),
        n(&[
            "/usage/completion_tokens",
            "/usage/output_tokens",
            "/usage/outputTokens",
            "/usageMetadata/candidatesTokenCount",
        ]),
    )
}

/// Records a completion test as a `test` ledger line that no budget is charged with.
fn record_test(call: &Call, d: &DeploymentRt, usage: Usage, ok: bool) {
    let now = call.rt.now();
    let price = call
        .snap
        .pricing
        .price(d.provider.kind.as_str(), &d.base_model, &d.name);
    let units = Units {
        requests: u64::from(ok),
        ..Units::default()
    };
    let (cost, billed, cost_status, version) = match price {
        Some(p) => match antirion_limits::cost::cost(&p, &usage, &units, None, Dec::ONE) {
            Ok(c) => (Some(c.total), Some(c.billed), c.status, Some(p.version())),
            Err(_) => (None, None, CostStatus::Estimated, Some(p.version())),
        },
        None => (Some(Nusd::ZERO), Some(Nusd::ZERO), CostStatus::Unpriced, None),
    };
    call.rt.record_spend(vec![LedgerLine {
        request_id: call.request_id.as_str().to_owned(),
        line: 0,
        purpose: Purpose::Test,
        shadow: false,
        status: if ok {
            LineStatus::Success
        } else {
            LineStatus::Failed
        },
        admitted_at: now,
        settled_at: now,
        organization: String::new(),
        team: String::new(),
        project: String::new(),
        key: String::new(),
        user: Some(call.principal.id.clone()),
        end_user: None,
        tags: Vec::new(),
        model: None,
        route: None,
        provider: Some(d.provider.name.clone()),
        deployment: Some(d.name.clone()),
        upstream_model: Some(d.model.clone()),
        call: CallTarget::default(),
        endpoint: "provider_test".to_owned(),
        stream: false,
        usage,
        usage_raw: None,
        usage_source: UsageSource::Upstream,
        cost,
        billed,
        cost_status,
        price_version: version,
        service_tier: None,
        avoided: None,
        reserved: Reserved {
            input: 0,
            output: 0,
            usd: None,
            out_source: OutSource::Request,
        },
        budgets: Vec::new(),
    }]);
}

/// `POST /providers/{name}:test`.
pub(crate) async fn test(call: &Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    call.require("providers:write", &[Scope::Gateway])?;
    let p = provider(call, name)?;
    let body = if call.body.is_empty() {
        Map::new()
    } else {
        members(call, &["deployment", "credential"])?
    };
    let mut of_provider: Vec<&std::sync::Arc<DeploymentRt>> = call
        .snap
        .providers
        .deployments
        .values()
        .filter(|d| d.provider.name == p.name)
        .collect();
    of_provider.sort_by(|a, b| a.name.cmp(&b.name));
    let d = match body.get("deployment").and_then(Value::as_str) {
        Some(n) => of_provider.into_iter().find(|d| d.name == n).ok_or_else(|| {
            invalid(
                "deployment",
                antirion_config::DetailCode::UnresolvedReference,
                "the deployment is not one of the provider's",
            )
        })?,
        None => of_provider.into_iter().next().ok_or_else(|| {
            invalid(
                "deployment",
                antirion_config::DetailCode::Required,
                "the provider has no deployment to test with",
            )
        })?,
    };
    let key = body.get("credential").and_then(Value::as_str);
    if let Some(k) = key
        && !p.keys.iter().any(|x| x.name == k)
    {
        return Err(invalid(
            "credential",
            antirion_config::DetailCode::UnresolvedReference,
            "the provider has no credential of that name",
        ));
    }
    let (op, probe_body) = probe(d);
    let completion = probe_body.is_some();
    let request_id = RequestId::generate();
    let prepared = d.build_request(UpstreamCall {
        op,
        stream: false,
        body: Payload::from(bytes::Bytes::from(
            probe_body.as_ref().map(Value::to_string).unwrap_or_default(),
        )),
        content_type: Some(HeaderValue::from_static("application/json")),
        request_id: &request_id,
        response_timeout: TEST_TIMEOUT,
        connect_timeout: None,
        anthropic: None,
        key,
        byok: None,
    });
    let started = Instant::now();
    let mut outcome = json!({"ok": false, "status_code": null, "latency_ms": null,
                             "upstream_request_id": null, "error": null});
    let result = match prepared {
        Err(_) => Err(("config", "The provider's request could not be built.".to_owned())),
        Ok(prepared) => {
            let mut request = prepared.request;
            request.resend_when_closed = false;
            if !completion {
                request.method = Method::GET;
                request.body = Payload::from(bytes::Bytes::new());
                request.headers.remove(http::header::CONTENT_TYPE);
            }
            let tls = call
                .snap
                .security
                .egress_tls
                .clone()
                .unwrap_or_else(|| p.tls.clone());
            match d.authorize(&mut request, &call.rt.creds, &tls).await {
                Err(_) => Err((
                    "auth",
                    "The gateway could not obtain the provider's credential.".to_owned(),
                )),
                Ok(_) => match tokio::time::timeout(TEST_TIMEOUT, call.rt.egress.send(request)).await {
                    Err(_) => Err(("timeout", "The provider did not answer in time.".to_owned())),
                    Ok(Err(e)) => Err(("connect", e.to_string())),
                    Ok(Ok(resp)) => {
                        let status = resp.status.as_u16();
                        let upstream_id = ["x-request-id", "request-id", "x-amzn-requestid"]
                            .iter()
                            .find_map(|h| resp.headers.get(*h).and_then(|v| v.to_str().ok()))
                            .map(str::to_owned);
                        let mut read = Vec::new();
                        let mut body = resp.body;
                        while let Some(Ok(frame)) = body.frame().await {
                            if let Ok(data) = frame.into_data() {
                                read.extend_from_slice(&data);
                                if read.len() >= TEST_BODY {
                                    break;
                                }
                            }
                        }
                        Ok((status, upstream_id, read))
                    }
                },
            }
        }
    };
    #[allow(clippy::cast_precision_loss)]
    let latency = (started.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
    if let Some(o) = outcome.as_object_mut() {
        o.insert("latency_ms".into(), json!(latency));
        match &result {
            Ok((status, upstream_id, read)) => {
                let ok = (200..300).contains(status);
                o.insert("ok".into(), json!(ok));
                o.insert("status_code".into(), json!(status));
                o.insert("upstream_request_id".into(), json!(upstream_id));
                if !ok {
                    let message = scrubbed(read, &p.secrets)
                        .unwrap_or_else(|| format!("The provider answered {status}."));
                    o.insert(
                        "error".into(),
                        json!({"class": class_of(*status), "message": message}),
                    );
                }
                if completion {
                    record_test(call, d, usage_of(read), ok);
                }
            }
            Err((class, message)) => {
                o.insert("error".into(), json!({"class": class, "message": message}));
            }
        }
    }
    let ok = outcome.get("ok") == Some(&Value::Bool(true));
    call.audit_with(
        "POST providers:test",
        &format!("providers/{name}"),
        if ok { Outcome::Success } else { Outcome::Failed },
        StatusCode::OK,
        json!({"deployment": d.name, "credential": key, "ok": ok}),
    )
    .await;
    Ok(call.json(StatusCode::OK, &outcome))
}

/// `POST /providers/{name}:rotate_credential`.
pub(crate) async fn rotate(call: &Call, name: &str) -> Result<Response, AdminError> {
    if call.method != Method::POST {
        return Err(AdminError::method_not_allowed("POST"));
    }
    call.allow_params(&[])?;
    call.require("providers:write", &[Scope::Gateway])?;
    let body = members(call, &["credential", "retire_after", "key"])?;
    provider(call, name)?;
    let credential = body
        .get("credential")
        .filter(|c| c.as_str().is_some_and(|s| !s.is_empty()) || c.is_object())
        .cloned()
        .ok_or_else(|| {
            invalid(
                "credential",
                antirion_config::DetailCode::Required,
                "credential is required",
            )
        })?;
    let overlap = body
        .get("retire_after")
        .and_then(Value::as_str)
        .and_then(antirion_config::scalar::parse_duration)
        .ok_or_else(|| {
            invalid(
                "retire_after",
                antirion_config::DetailCode::InvalidFormat,
                "retire_after is a duration",
            )
        })?;
    if overlap > MAX_OVERLAP {
        return Err(invalid(
            "retire_after",
            antirion_config::DetailCode::OutOfRange,
            "retire_after is at most 7d",
        ));
    }
    let key = body.get("key").and_then(Value::as_str).map(str::to_owned);
    let now = call.rt.now();
    let standby = format!(
        "{}-retiring-{}",
        key.as_deref().unwrap_or(antirion_providers::SINGLE_KEY),
        now.0.div_euclid(1_000_000)
    );
    let resource = format!("providers/{name}");
    let (provider_name, standby_name) = (name.to_owned(), standby.clone());
    let committed = api_write(
        call,
        "POST providers:rotate_credential",
        &resource,
        StatusCode::OK,
        |state| {
            let mut candidate = state.doc.clone();
            let auth = candidate
                .pointer_mut(&format!("/providers/{provider_name}/auth"))
                .and_then(Value::as_object_mut)
                .ok_or_else(|| {
                    invalid(
                        "credential",
                        antirion_config::DetailCode::Conflict,
                        "the provider has no API-key credential to rotate",
                    )
                })?;
            let old = match &key {
                None => {
                    let old = auth.remove("api_key").ok_or_else(|| {
                        invalid(
                            "credential",
                            antirion_config::DetailCode::Conflict,
                            "the provider has no auth.api_key; name the key to rotate",
                        )
                    })?;
                    auth.insert(
                        "api_keys".into(),
                        json!([{"name": antirion_providers::SINGLE_KEY, "key": credential.clone()}]),
                    );
                    old
                }
                Some(k) => {
                    let entry = auth
                        .get_mut("api_keys")
                        .and_then(Value::as_array_mut)
                        .and_then(|keys| {
                            keys.iter_mut()
                                .find(|e| e.get("name").and_then(Value::as_str) == Some(k.as_str()))
                        })
                        .and_then(Value::as_object_mut)
                        .ok_or_else(|| {
                            invalid(
                                "key",
                                antirion_config::DetailCode::UnresolvedReference,
                                "the provider has no key of that name",
                            )
                        })?;
                    entry
                        .insert("key".into(), credential.clone())
                        .unwrap_or(Value::Null)
                }
            };
            let keys = auth
                .entry("api_keys")
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .ok_or_else(AdminError::internal)?;
            keys.push(json!({"name": standby_name, "key": old, "weight": 0}));
            Ok(Proposal {
                candidate,
                written: vec![Target::Resource("providers", provider_name.clone())],
                credentials: Vec::new(),
                meta_edits: Vec::new(),
            })
        },
    )
    .await?;
    let retire_at = now.saturating_add(overlap);
    if call
        .rt
        .schedule_retirement(name, &standby, retire_at)
        .await
        .is_err()
    {
        return Err(AdminError::store_unavailable());
    }
    let snap = call.rt.gens.load();
    let shown = antirion_runtime::snapshot::redacted(&snap.config.doc, &snap.secret_paths);
    let spec = shown
        .pointer(&format!("/providers/{name}"))
        .cloned()
        .unwrap_or(Value::Null);
    let meta = snap
        .config
        .meta
        .resources
        .get("providers")
        .and_then(|m| m.get(name));
    let mut item = crate::resources::representation(name, &spec, meta, snap.generation);
    crate::hints::decorate(&snap, name, &mut item);
    decorate(&snap, name, &mut item, call.rt.now());
    if let Some(m) = item.get_mut("metadata").and_then(Value::as_object_mut) {
        m.insert(
            "retires_at".into(),
            json!({standby: retire_at.to_rfc3339_seconds()}),
        );
    }
    let resp = call.json(StatusCode::OK, &item);
    Ok(call.mutated(resp, committed.generation).await)
}

/// Adds `metadata.credential_rotated_at` and `metadata.rotation_due` to a provider's
/// representation.
pub(crate) fn decorate(snap: &antirion_runtime::Snapshot, name: &str, item: &mut Value, now: UtcMicros) {
    let Some(meta) = snap
        .config
        .meta
        .resources
        .get("providers")
        .and_then(|m| m.get(name))
    else {
        return;
    };
    let max_age = snap
        .config
        .doc
        .pointer(&format!("/providers/{name}/credential_rotation_max_age"))
        .and_then(Value::as_str)
        .and_then(antirion_config::scalar::parse_duration);
    let due = max_age.is_some_and(|max| {
        meta.credentials_rotated_at
            .values()
            .any(|at| UtcMicros::parse_rfc3339(at).is_ok_and(|t| now.since(t) > max))
    });
    if let Some(m) = item.get_mut("metadata").and_then(Value::as_object_mut) {
        m.insert("credential_rotated_at".into(), json!(meta.credentials_rotated_at));
        m.insert("rotation_due".into(), json!(due));
    }
}

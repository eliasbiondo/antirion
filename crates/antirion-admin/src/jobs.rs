//! Export jobs: `POST /organizations/{name}:export` starts one, `GET /jobs/{id}` follows it and
//! `GET /jobs/{id}/download` hands out its ZIP. A job and its file are rows of the shared store, so
//! any node answers for them; the job itself runs on the node that accepted it.

use std::sync::Arc;

use antirion_config::model::DEFAULT_TENANT;
use antirion_core::{RequestId, UtcMicros};
use antirion_limits::book::LedgerFilter;
use antirion_runtime::operations::{FILE_LIFETIME, JOB, JOB_FILE};
use antirion_runtime::reqlog::joined;
use antirion_runtime::{Runtime, Snapshot};
use antirion_store::logs::{Arg, LogColumn, LogFilter, LogOrder, SortKey};
use antirion_store::{AuditQuery, PendingRequest};
use axum::body::Body;
use axum::response::Response;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use http::{HeaderValue, Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::call::Call;
use crate::danger_routes::{body, only, organization};
use crate::error::AdminError;
use crate::zipfile::ZipWriter;

/// The most request-log records, ledger lines and audit entries an export holds, newest first.
const MAX_ROWS: usize = 100_000;
/// Records and audit entries read per store call.
const BATCH: usize = 1_000;

/// A job as the contract shows it.
fn shown(job: &Value, now: UtcMicros) -> Value {
    let mut v = job.clone();
    let expired = v["expires_at"]
        .as_str()
        .and_then(|t| UtcMicros::parse_rfc3339(t).ok())
        .is_some_and(|t| now >= t);
    if let Some(o) = v.as_object_mut() {
        o.remove("organization");
        if expired {
            o.insert("download_url".into(), Value::Null);
        }
    }
    v
}

/// A job's row, when `call` created it.
async fn own(call: &Call, id: &str) -> Result<(PendingRequest, Value), AdminError> {
    let row = call
        .rt
        .store
        .request(JOB, id)
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .filter(|r| r.author == call.principal.id)
        .ok_or_else(AdminError::not_found)?;
    let job = serde_json::from_str(&row.body).map_err(|_| AdminError::internal())?;
    Ok((row, job))
}

/// `GET /jobs/{id}`: only the job's creator reads it; anyone else gets 404.
pub(crate) async fn get(call: &Call, id: &str) -> Result<Response, AdminError> {
    only(call, Method::GET, "GET")?;
    call.allow_params(&[])?;
    let (_, job) = own(call, id).await?;
    Ok(call.json(StatusCode::OK, &shown(&job, call.rt.now())))
}

/// `GET /jobs/{id}/download`.
pub(crate) async fn download(call: &Call, id: &str) -> Result<Response, AdminError> {
    only(call, Method::GET, "GET")?;
    call.allow_params(&[])?;
    let (_, job) = own(call, id).await?;
    if shown(&job, call.rt.now())["download_url"].is_null() {
        return Err(AdminError::not_found());
    }
    let file = call
        .rt
        .store
        .request(JOB_FILE, id)
        .await
        .map_err(|_| AdminError::store_unavailable())?
        .ok_or_else(AdminError::not_found)?;
    let bytes = STANDARD
        .decode(file.body.as_bytes())
        .map_err(|_| AdminError::internal())?;
    let org = job["organization"].as_str().unwrap_or("organization");
    call.audit_read("jobs.download", &format!("jobs/{id}")).await;
    let mut resp = Response::new(Body::from(bytes));
    let h = resp.headers_mut();
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/zip"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{org}-export-{id}.zip\"")) {
        h.insert(CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

/// What an export may include, decided from the caller's permissions when it starts.
struct Export {
    rt: Arc<Runtime>,
    snap: Arc<Snapshot>,
    organization: String,
    job: Value,
    version: i64,
    content: bool,
}

/// `POST /organizations/{name}:export`.
pub(crate) async fn export(call: &Call, name: &str) -> Result<Response, AdminError> {
    only(call, Method::POST, "POST")?;
    call.allow_params(&[])?;
    let resource = format!("organizations/{name}");
    let chain = organization(call, name)?;
    for p in ["organizations:read", "config:read", "logs:read"] {
        call.require(p, &chain).map_err(|e| e.with_resource(&resource))?;
    }
    body(call, &[])?;
    let now = call.rt.now();
    let id = format!("job_{}", RequestId::generate().as_str());
    let job = json!({
        "id": id,
        "kind": "organization_export",
        "state": "running",
        "progress": 0.0,
        "created_at": now.to_rfc3339(),
        "download_url": null,
        "expires_at": null,
        "error": null,
        "organization": name,
    });
    call.rt
        .store
        .insert_request(PendingRequest {
            kind: JOB.to_owned(),
            id: id.clone(),
            subject: name.to_owned(),
            state: "running".to_owned(),
            author: call.principal.id.clone(),
            created_us: now.0,
            version: 0,
            body: job.to_string(),
        })
        .await
        .map_err(|_| AdminError::store_unavailable())?;
    call.audit_read("organizations.export", &resource).await;
    let content = call.principal.allows("logs:read_content", &chain);
    let run = Export {
        rt: Arc::clone(&call.rt),
        snap: Arc::clone(&call.snap),
        organization: name.to_owned(),
        job: job.clone(),
        version: 0,
        content,
    };
    call.rt.tasks.spawn(Box::pin(run.run()));
    Ok(call.json(StatusCode::ACCEPTED, &shown(&job, now)))
}

/// One ZIP member's lines.
fn ndjson<'a>(rows: impl Iterator<Item = &'a Value>) -> Vec<u8> {
    let mut out = Vec::new();
    for r in rows {
        out.extend_from_slice(r.to_string().as_bytes());
        out.push(b'\n');
    }
    out
}

impl Export {
    async fn run(mut self) {
        let result = self.build().await;
        let now = self.rt.now();
        match result {
            Ok(file) => {
                let expires = now.saturating_add(FILE_LIFETIME);
                let id = self.job["id"].as_str().unwrap_or_default().to_owned();
                let stored = self
                    .rt
                    .store
                    .insert_request(PendingRequest {
                        kind: JOB_FILE.to_owned(),
                        id: id.clone(),
                        subject: self.organization.clone(),
                        state: "ready".to_owned(),
                        author: String::new(),
                        created_us: expires.0,
                        version: 0,
                        body: STANDARD.encode(file),
                    })
                    .await;
                if stored.is_err() {
                    self.finish("failed", Some("the export file could not be stored"))
                        .await;
                    return;
                }
                self.job["download_url"] = json!(format!("/admin/v1/jobs/{id}/download"));
                self.job["expires_at"] = json!(expires.to_rfc3339());
                self.job["progress"] = json!(1.0);
                self.finish("succeeded", None).await;
            }
            Err(message) => self.finish("failed", Some(message)).await,
        }
    }

    async fn finish(&mut self, state: &str, error: Option<&str>) {
        self.job["state"] = json!(state);
        self.job["error"] = json!(error);
        self.save().await;
    }

    async fn progress(&mut self, done: f64) {
        self.job["progress"] = json!(done);
        self.save().await;
    }

    async fn save(&mut self) {
        let id = self.job["id"].as_str().unwrap_or_default().to_owned();
        let state = self.job["state"].as_str().unwrap_or("running").to_owned();
        match self
            .rt
            .store
            .update_request(JOB, &id, self.version, &state, &self.job.to_string())
            .await
        {
            Ok(true) => self.version += 1,
            Ok(false) => tracing::warn!(job = %id, "an export job was changed by another writer"),
            Err(e) => tracing::warn!(job = %id, error = %e, "an export job's progress could not be stored"),
        }
    }

    async fn build(&mut self) -> Result<Vec<u8>, &'static str> {
        let (config, scope) = self.config();
        self.progress(0.1).await;
        let audit = self.audit(&scope).await?;
        self.progress(0.3).await;
        let ledger: Vec<Value> = self
            .rt
            .spend_lines(&LedgerFilter {
                organization: Some(self.organization.clone()),
                ..LedgerFilter::default()
            })
            .await
            .iter()
            .rev()
            .take(MAX_ROWS)
            .map(|l| l.to_json())
            .collect();
        self.progress(0.5).await;
        let logs = self.logs().await?;
        self.progress(0.9).await;
        let manifest = json!({
            "organization": self.organization,
            "exported_at": self.rt.now().to_rfc3339(),
            "config_generation": self.snap.generation,
            "content_included": self.content,
            "counts": {"audit": audit.len(), "ledger": ledger.len(), "request_logs": logs.len()},
            "limit": MAX_ROWS,
        });
        let mut zip = ZipWriter::default();
        let too_large = |_| "the export is too large";
        zip.add("manifest.json", manifest.to_string().as_bytes())
            .map_err(too_large)?;
        zip.add("config.json", config.to_string().as_bytes())
            .map_err(too_large)?;
        zip.add("audit.ndjson", &ndjson(audit.iter()))
            .map_err(too_large)?;
        zip.add("ledger.ndjson", &ndjson(ledger.iter()))
            .map_err(too_large)?;
        zip.add("request_logs.ndjson", &ndjson(logs.iter()))
            .map_err(too_large)?;
        zip.finish().map_err(too_large)
    }

    /// The organization's part of the document, secrets redacted, and the audit resources of its
    /// subtree. Users appear with only their role bindings inside the organization.
    fn config(&self) -> (Value, Vec<String>) {
        let doc = antirion_config::secrets::redact(&self.snap.config.doc, &self.snap.secret_paths);
        let org = self.organization.as_str();
        let (teams, projects, keys) = antirion_runtime::operations::deletion::subtree(&doc, org);
        let pick = |collection: &str, names: &[String]| -> Map<String, Value> {
            names
                .iter()
                .filter_map(|n| {
                    doc.get(collection)
                        .and_then(|c| c.get(n))
                        .map(|s| (n.clone(), s.clone()))
                })
                .collect()
        };
        let mut organizations = pick("organizations", std::slice::from_ref(&self.organization));
        if org == DEFAULT_TENANT && organizations.is_empty() {
            organizations.insert(org.to_owned(), json!({}));
        }
        let scopes: Vec<String> = std::iter::once(format!("organization/{org}"))
            .chain(teams.iter().map(|t| format!("team/{t}")))
            .chain(projects.iter().map(|p| format!("project/{p}")))
            .collect();
        let users: Map<String, Value> = doc
            .get("users")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(name, spec)| {
                let inside: Vec<Value> = spec
                    .get("role_bindings")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|b| {
                        b.get("scope")
                            .and_then(Value::as_str)
                            .is_some_and(|s| scopes.iter().any(|x| x == s))
                    })
                    .cloned()
                    .collect();
                if inside.is_empty() {
                    return None;
                }
                let mut spec = spec.clone();
                spec["role_bindings"] = Value::Array(inside);
                Some((name.clone(), spec))
            })
            .collect();
        let config = json!({
            "organizations": organizations,
            "teams": pick("teams", &teams),
            "projects": pick("projects", &projects),
            "keys": pick("keys", &keys),
            "users": users,
        });
        let resources = std::iter::once(format!("organizations/{org}"))
            .chain(teams.iter().map(|t| format!("teams/{t}")))
            .chain(projects.iter().map(|p| format!("projects/{p}")))
            .chain(keys.iter().map(|k| format!("keys/{k}")))
            .collect();
        (config, resources)
    }

    /// The audit entries of the subtree's resources, newest first.
    async fn audit(&self, resources: &[String]) -> Result<Vec<Value>, &'static str> {
        let mut out = Vec::new();
        for r in resources {
            let mut before = None;
            loop {
                let page = self
                    .rt
                    .store
                    .audit(AuditQuery {
                        resource: Some(r.clone()),
                        before,
                        limit: BATCH,
                        ..AuditQuery::default()
                    })
                    .await
                    .map_err(|_| "the audit log could not be read")?;
                out.extend(page.items.iter().filter_map(|e| serde_json::to_value(e).ok()));
                match page.next {
                    Some(n) if out.len() < MAX_ROWS => before = Some(n),
                    _ => break,
                }
            }
        }
        out.sort_by(|a, b| b["time"].as_str().cmp(&a["time"].as_str()));
        out.truncate(MAX_ROWS);
        Ok(out)
    }

    /// The organization's request-log records, newest first, content only when the caller may
    /// read it.
    async fn logs(&self) -> Result<Vec<Value>, &'static str> {
        let filter = LogFilter {
            any_of: vec![(
                LogColumn::Organization,
                vec![Arg::Text(self.organization.clone())],
            )],
            ..LogFilter::default()
        };
        let mut order = LogOrder {
            sort: SortKey::StartedAt,
            desc: true,
            after: None,
            limit: BATCH,
        };
        let mut out = Vec::new();
        loop {
            let batch = self
                .rt
                .list_logs(filter.clone(), order.clone())
                .await
                .map_err(|_| "the request log could not be read")?;
            for l in &batch {
                let mut record = joined(&l.log);
                if !self.content {
                    crate::logs::redact(&mut record);
                }
                out.push(record);
            }
            if batch.len() < BATCH || out.len() >= MAX_ROWS {
                break;
            }
            order.after = batch.last().map(|l| l.pos.clone());
        }
        out.truncate(MAX_ROWS);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expired_job_has_no_download() {
        let job = json!({"id": "j", "state": "succeeded", "download_url": "/admin/v1/jobs/j/download",
                         "expires_at": "1970-01-01T00:00:10Z", "organization": "acme"});
        let before = shown(&job, UtcMicros(9_000_000));
        assert_eq!(before["download_url"], "/admin/v1/jobs/j/download");
        assert!(before.get("organization").is_none());
        assert!(shown(&job, UtcMicros(10_000_000))["download_url"].is_null());
    }

    #[test]
    fn ndjson_is_one_line_per_row() {
        let rows = [json!({"a": 1}), json!({"b": 2})];
        assert_eq!(ndjson(rows.iter()), b"{\"a\":1}\n{\"b\":2}\n".to_vec());
    }
}

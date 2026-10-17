//! `GET /logs:export`: the records a log query selects, streamed as CSV (one header row of the
//! record's scalar fields, never content) or NDJSON (whole records, content only for readers who may
//! see it), at most `request_logs.export_max_rows`. The export is audited with its filters and the
//! number of rows once the stream ends.

use std::convert::Infallible;
use std::sync::Arc;

use antirion_core::RequestId;
use antirion_core::audit::{Actor, AuditEntry, Outcome};
use antirion_identity::rbac::AdminPrincipal;
use antirion_runtime::Runtime;
use antirion_runtime::reqlog::analytics::metadata_matches;
use antirion_runtime::reqlog::joined;
use antirion_store::logs::LogOrder;
use axum::body::Body;
use axum::response::Response;
use bytes::Bytes;
use http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use http::{HeaderValue, Method};
use serde_json::{Value, json};

use crate::call::Call;
use crate::error::AdminError;
use crate::logquery::{self, SELECTION, Selection};
use crate::logs::{order_of, redact, require_some, scope_of, searches_content};

/// Records read per store call.
const BATCH: usize = 1_000;

/// The CSV columns: the record's scalar fields, by JSON pointer.
const COLUMNS: &[&str] = &[
    "id",
    "request_id",
    "trace_id",
    "span_id",
    "node_id",
    "gateway_version",
    "config_generation",
    "started_at",
    "ended_at",
    "method",
    "endpoint",
    "operation",
    "dialect",
    "stream.end_reason",
    "stream.committed",
    "stream.relay_mode",
    "client_ip",
    "user_agent",
    "client_retry_count",
    "organization",
    "team",
    "project",
    "key",
    "end_user",
    "session_id",
    "requested_model",
    "model",
    "route",
    "provider",
    "deployment",
    "upstream_model",
    "response_model",
    "upstream_request_id",
    "response_path",
    "status_code",
    "code",
    "reason",
    "stage",
    "client_disconnected",
    "attempt_count",
    "cache.status",
    "cache.layer",
    "usage.input",
    "usage.input_cache_read",
    "usage.input_cache_write",
    "usage.output",
    "usage.output_reasoning",
    "usage.total",
    "usage.source",
    "cost.currency",
    "cost.total",
    "cost.status",
    "timing.total_ms",
    "timing.overhead_ms",
    "timing.upstream_ms",
    "timing.ttft_ms",
    "tool_call_count",
    "replay_of",
    "client_geo.country",
];

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Format {
    Csv,
    Ndjson,
}

/// One CSV field (RFC 4180): quoted when it holds a comma, quote or line break.
pub(crate) fn field(v: Option<&Value>) -> String {
    let text = match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

fn csv_row(record: &Value) -> String {
    let row: Vec<String> = COLUMNS
        .iter()
        .map(|c| field(record.pointer(&format!("/{}", c.replace('.', "/")))))
        .collect();
    format!("{}\r\n", row.join(","))
}

struct Export {
    rt: Arc<Runtime>,
    viewer: AdminPrincipal,
    sel: Selection,
    order: LogOrder,
    format: Format,
    left: u64,
    rows: u64,
    header: bool,
    done: bool,
    audit: Option<AuditEntry>,
}

impl Export {
    fn render(&mut self, record: &Value) -> Option<String> {
        let chain = scope_of(record);
        if !self.viewer.sees(&chain) || !self.viewer.allows("logs:read", &chain) {
            return None;
        }
        if !metadata_matches(record, &self.sel.metadata) {
            return None;
        }
        Some(match self.format {
            Format::Csv => csv_row(record),
            Format::Ndjson => {
                let mut r = record.clone();
                if !self.viewer.allows("logs:read_content", &chain) {
                    redact(&mut r);
                }
                format!("{r}\n")
            }
        })
    }

    async fn finish(&mut self) {
        if let Some(mut entry) = self.audit.take() {
            if let Some(o) = entry.changes.as_object_mut() {
                o.insert("rows".into(), json!(self.rows));
            }
            if let Err(e) = self.rt.store.append_audit(entry).await {
                tracing::warn!(error = %e, "an audit entry could not be written");
            }
        }
    }

    /// The next chunk of the export, `None` at its end.
    async fn next(&mut self) -> Option<Bytes> {
        if !self.header {
            self.header = true;
            if self.format == Format::Csv {
                return Some(Bytes::from(format!("{}\r\n", COLUMNS.join(","))));
            }
        }
        while !self.done {
            if self.left == 0 {
                self.done = true;
                break;
            }
            self.order.limit = BATCH;
            let Ok(batch) = self
                .rt
                .list_logs(self.sel.filter.clone(), self.order.clone())
                .await
            else {
                self.done = true;
                break;
            };
            if batch.len() < BATCH {
                self.done = true;
            }
            self.order.after = batch.last().map(|l| l.pos.clone());
            let mut chunk = String::new();
            for l in &batch {
                if self.left == 0 {
                    break;
                }
                if let Some(line) = self.render(&joined(&l.log)) {
                    chunk.push_str(&line);
                    self.left -= 1;
                    self.rows += 1;
                }
            }
            if !chunk.is_empty() {
                return Some(Bytes::from(chunk));
            }
        }
        self.finish().await;
        None
    }
}

/// `GET /logs:export`.
pub(crate) fn export(call: &Call) -> Result<Response, AdminError> {
    if call.method != Method::GET {
        return Err(AdminError::method_not_allowed("GET"));
    }
    logquery::allow(call, SELECTION, &["sort", "order", "format"])?;
    require_some(call, "logs:read")?;
    let format = match call.param("format") {
        None | Some("csv") => Format::Csv,
        Some("ndjson") => Format::Ndjson,
        Some(_) => return Err(AdminError::invalid_parameter("format must be csv or ndjson")),
    };
    let (sort, desc) = order_of(call)?;
    let sel = logquery::parse(call, "logs:read", searches_content(call))?;
    let filters: serde_json::Map<String, Value> = call
        .query
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    let generation = call.snap.generation;
    let audit = AuditEntry {
        id: RequestId::generate().as_str().to_owned(),
        time: call.rt.now().to_rfc3339(),
        actor: Actor {
            kind: call.principal.kind,
            id: Some(call.principal.id.clone()),
        },
        action: "logs.export".to_owned(),
        resource: "logs".to_owned(),
        outcome: Outcome::Success,
        status_code: 200,
        generation_before: Some(generation),
        generation_after: Some(generation),
        request_id: call.request_id.as_str().to_owned(),
        source_ip: call.source_ip.clone(),
        changes: json!({"filters": filters, "format": if format == Format::Csv { "csv" } else { "ndjson" }}),
        kind: antirion_core::audit::AuditKind::Read,
        reason: None,
    };
    let export = Export {
        rt: Arc::clone(&call.rt),
        viewer: call.principal.clone(),
        sel,
        order: LogOrder {
            sort,
            desc,
            after: None,
            limit: BATCH,
        },
        format,
        left: call.snap.obs.request_logs.export_max_rows,
        rows: 0,
        header: false,
        done: false,
        audit: Some(audit),
    };
    let stream = futures_util::stream::unfold(export, |mut e| async move {
        e.next().await.map(|chunk| (Ok::<_, Infallible>(chunk), e))
    });
    let (content_type, name) = match format {
        Format::Csv => ("text/csv; charset=utf-8", "request-logs.csv"),
        Format::Ndjson => ("application/x-ndjson", "request-logs.ndjson"),
    };
    let mut resp = Response::new(Body::from_stream(stream));
    let h = resp.headers_mut();
    h.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        h.insert(CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_rows_quote_and_flatten() {
        let r = json!({"id": "a,b", "model": "m\"x", "usage": {"input": 3}, "cost": {"total": "0.1"},
            "content": {"request": "secret prompt"}});
        let row = csv_row(&r);
        assert!(row.starts_with("\"a,b\","));
        assert!(row.contains("\"m\"\"x\""));
        assert!(!row.contains("secret prompt"));
        assert!(row.ends_with("\r\n"));
        assert_eq!(
            row.matches(',').count(),
            COLUMNS.len(),
            "the separators and the comma inside \"a,b\""
        );
    }
}

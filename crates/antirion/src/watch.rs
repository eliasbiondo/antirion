//! The watched config file (`admin.config.file` with `watch: true`): a change is applied after the
//! debounce like any apply of the configured source; invalid content and deletions change nothing,
//! and the process never exits because of the file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use antirion_config::Format;
use antirion_core::RequestId;
use antirion_core::audit::{Actor, ActorKind, AuditEntry, Outcome};
use antirion_runtime::Runtime;
use serde_json::{Map, Value};

use crate::startup::{apply_document, dynamic_keys, render_errors};

/// How often the file is looked at.
const POLL: Duration = Duration::from_millis(100);

/// The startup document's dynamic keys overlaid with the file's, by top-level key.
pub(crate) async fn compose(base: &Map<String, Value>, path: &Path) -> Result<Map<String, Value>, String> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    parse_over(base, path, &bytes)
}

fn parse_over(base: &Map<String, Value>, path: &Path, bytes: &[u8]) -> Result<Map<String, Value>, String> {
    let doc = antirion_config::parse(bytes, Format::from_file_name(&path.to_string_lossy()))
        .map_err(|e| render_errors(&e))?;
    let mut out = base.clone();
    out.extend(dynamic_keys(&doc));
    Ok(out)
}

pub(crate) struct Watcher {
    pub(crate) rt: Arc<Runtime>,
    pub(crate) path: PathBuf,
    pub(crate) base: Map<String, Value>,
    pub(crate) debounce: Duration,
}

impl Watcher {
    async fn audit_rejection(&self) {
        let generation = self.rt.gens.load().generation;
        let entry = AuditEntry {
            id: RequestId::generate().as_str().to_owned(),
            time: self.rt.now().to_rfc3339(),
            actor: Actor {
                kind: ActorKind::System,
                id: Some("config-file".to_owned()),
            },
            action: "config.apply".to_owned(),
            resource: "config".to_owned(),
            outcome: Outcome::Failed,
            status_code: 400,
            generation_before: Some(generation),
            generation_after: Some(generation),
            request_id: RequestId::generate().as_str().to_owned(),
            source_ip: None,
            kind: antirion_core::audit::AuditKind::Write,
            reason: None,
            changes: Value::Null,
        };
        if let Err(e) = self.rt.store.append_audit(entry).await {
            tracing::warn!(error = %e, "an audit entry could not be written");
        }
    }

    pub(crate) async fn run(self) {
        let mut applied = tokio::fs::read(&self.path).await.ok();
        let stop = self.rt.shutdown.clone();
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(POLL) => {}
            }
            let Ok(current) = tokio::fs::read(&self.path).await else {
                continue;
            };
            if applied.as_ref() == Some(&current) {
                continue;
            }
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(self.debounce) => {}
            }
            let Ok(settled) = tokio::fs::read(&self.path).await else {
                continue;
            };
            if settled != current {
                continue;
            }
            let outcome = match parse_over(&self.base, &self.path, &settled) {
                Ok(doc) => apply_document(&self.rt, &doc, "config-file")
                    .await
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            match outcome {
                Ok(()) => tracing::info!(path = %self.path.display(), "applied the changed config file"),
                Err(reason) => {
                    tracing::warn!(path = %self.path.display(), "config.rejected: {reason}");
                    self.audit_rejection().await;
                }
            }
            applied = Some(settled);
        }
    }
}

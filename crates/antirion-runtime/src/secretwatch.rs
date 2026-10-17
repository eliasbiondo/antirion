//! Secret files behind `${file:…}` references, and the client certificates of providers' `tls`
//! blocks, are re-read while the node runs: when one changes, the current generation's snapshot is rebuilt
//! with the new values, without a new config generation. An unreadable or empty file leaves the
//! values in use unchanged.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{Runtime, snapshot};

/// How often the referenced files are read.
const POLL: Duration = Duration::from_secs(1);

fn file_refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => {
            let mut rest = s.as_str();
            while let Some(i) = rest.find("${file:") {
                let tail = rest.get(i + 7..).unwrap_or_default();
                let Some(end) = tail.find('}') else { break };
                out.push(tail.get(..end).unwrap_or_default().to_owned());
                rest = tail.get(end..).unwrap_or_default();
            }
        }
        Value::Array(a) => a.iter().for_each(|x| file_refs(x, out)),
        Value::Object(o) => o.values().for_each(|x| file_refs(x, out)),
        _ => {}
    }
}

/// The client certificate and key files of the providers' `tls` blocks.
fn tls_files(doc: &Value, out: &mut Vec<String>) {
    for p in doc
        .get("providers")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|m| m.values())
    {
        for k in ["client_cert_file", "client_key_file"] {
            if let Some(f) = p.get("tls").and_then(|t| t.get(k)).and_then(Value::as_str) {
                out.push(f.to_owned());
            }
        }
    }
}

/// A digest of every referenced file, or `None` when one is unreadable or empty.
async fn digest(paths: &[String]) -> Option<[u8; 32]> {
    let mut h = Sha256::new();
    for p in paths {
        let bytes = tokio::fs::read(p).await.ok()?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return None;
        }
        h.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
        h.update(&bytes);
    }
    Some(h.finalize().into())
}

/// Watches the files until shutdown.
pub async fn run(rt: Arc<Runtime>) {
    let mut seen: Option<(u64, [u8; 32])> = None;
    loop {
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = tokio::time::sleep(POLL) => {}
        }
        let snap = rt.gens.load();
        let mut paths = Vec::new();
        file_refs(&snap.config.doc, &mut paths);
        tls_files(&snap.config.doc, &mut paths);
        if paths.is_empty() {
            seen = None;
            continue;
        }
        paths.sort();
        paths.dedup();
        let Some(d) = digest(&paths).await else { continue };
        // A generation seen for the first time is rebuilt once: its files may have changed between
        // its build and this read.
        if seen.is_some_and(|(generation, prev)| generation == snap.generation && prev == d) {
            continue;
        }
        if rebuild(&rt, snap.generation, "credentials re-read from their files").await {
            seen = Some((snap.generation, d));
        }
    }
}

/// Rebuilds the snapshot of generation `generation` with the secret values as they are now, when it
/// is still the serving one; true when the rebuilt snapshot is serving. Values that cannot be
/// applied leave the previous ones in use.
pub(crate) async fn rebuild(rt: &Arc<Runtime>, generation: u64, what: &str) -> bool {
    let _guard = rt.write_lock.lock().await;
    let current = rt.gens.load();
    if current.generation != generation {
        return false;
    }
    let state = (*current.config).clone();
    let secret_paths = (*current.secret_paths).clone();
    let secrets = Arc::clone(&rt.secrets);
    let pepper = rt.pepper.clone();
    let built =
        tokio::task::spawn_blocking(move || snapshot::build(state, secret_paths, secrets.as_ref(), &pepper))
            .await;
    match built {
        Ok(Ok(next)) => {
            let next = Arc::new(next);
            rt.gens.swap(Arc::clone(&next));
            rt.health_follows(&current, &next);
            rt.observe_generation(&next);
            tracing::debug!("{what}");
            true
        }
        _ => {
            tracing::warn!("{what}, but they could not be applied; the previous values stay in use");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn finds_file_references() {
        let mut out = Vec::new();
        file_refs(
            &json!({"a": "Bearer ${file:/run/k}", "b": ["${file:/x}${file:/y}"], "c": 1}),
            &mut out,
        );
        assert_eq!(out, ["/run/k", "/x", "/y"]);
        let mut tls = Vec::new();
        tls_files(
            &json!({"providers": {"p": {"tls": {"client_cert_file": "/c.pem", "client_key_file": "/c.key"}}, "q": {}}}),
            &mut tls,
        );
        assert_eq!(tls, ["/c.pem", "/c.key"]);
    }
}

//! The node's local snapshot: the last configuration it confirmed against the store, persisted in
//! the data directory so that it can start and serve while the store is unreachable. Secrets stay
//! sealed as they are in the store; the MCP session key is never written.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use antirion_core::UtcMicros;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::pause;
use crate::Runtime;
use crate::atrest::{SealError, Sealer};
use crate::snapshot::Snapshot;
use crate::state::{ConfigState, Metadata};

const FILE: &str = "cluster-snapshot.json";

/// The sealed pepper's location, as in the store.
const PEPPER_AT: &str = "node_values/key_pepper";

/// How often the persist time is renewed while the configuration is confirmed.
const RENEW: Duration = Duration::from_secs(10);

/// Why the local snapshot could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum LocalError {
    #[error("the local snapshot cannot be read or written: {0}")]
    Io(#[from] std::io::Error),
    #[error("the local snapshot is corrupt: {0}")]
    Corrupt(#[from] serde_json::Error),
    #[error(transparent)]
    Seal(#[from] SealError),
}

#[derive(Serialize, Deserialize)]
struct Persisted {
    persisted_at_us: i64,
    generation: u64,
    document: Value,
    metadata: Metadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pepper: Option<String>,
}

/// A local snapshot read at startup.
#[derive(Debug)]
pub struct Loaded {
    pub state: ConfigState,
    /// When the node last confirmed this configuration against the store.
    pub persisted_at: UtcMicros,
    /// The generated key pepper, when the startup file configures none.
    pub pepper: Option<Vec<u8>>,
}

/// Where a node persists its snapshot, and the sealed pepper it writes with it.
#[derive(Clone, Debug)]
pub struct SnapshotFile {
    path: PathBuf,
    pepper: Option<String>,
}

impl SnapshotFile {
    /// The snapshot file of the data directory `dir`; `pepper` is the generated key pepper.
    pub fn new(dir: &Path, sealer: &Sealer, pepper: Option<&[u8]>) -> Result<SnapshotFile, SealError> {
        Ok(SnapshotFile {
            path: dir.join(FILE),
            pepper: pepper.map(|p| sealer.seal(PEPPER_AT, p)).transpose()?,
        })
    }

    /// The snapshot persisted in `dir`, if there is one.
    pub fn read(dir: &Path, sealer: &Sealer) -> Result<Option<Loaded>, LocalError> {
        let bytes = match std::fs::read(dir.join(FILE)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let p: Persisted = serde_json::from_slice(&bytes)?;
        let mut doc = p.document;
        sealer.open_document(&mut doc)?;
        let pepper = p.pepper.map(|s| sealer.open(PEPPER_AT, &s)).transpose()?;
        Ok(Some(Loaded {
            state: ConfigState {
                generation: p.generation,
                doc,
                meta: p.metadata,
            },
            persisted_at: UtcMicros(p.persisted_at_us),
            pepper,
        }))
    }

    /// Writes `snap` as confirmed at `at`, replacing the previous file in one step.
    pub async fn write(&self, sealer: &Sealer, snap: &Snapshot, at: UtcMicros) -> Result<(), LocalError> {
        let persisted = Persisted {
            persisted_at_us: at.0,
            generation: snap.generation,
            document: sealer.seal_document(&snap.config.doc, &snap.secret_paths)?,
            metadata: snap.config.meta.clone(),
            pepper: self.pepper.clone(),
        };
        let bytes = serde_json::to_vec(&persisted)?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || replace(&path, &bytes))
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))??;
        Ok(())
    }
}

fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut f = private_file(&tmp)?;
    f.write_all(bytes)?;
    f.sync_data()?;
    std::fs::rename(&tmp, path)
}

#[cfg(unix)]
fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::create(path)
}

/// What was last written to the local snapshot.
#[derive(Debug, Default)]
pub(crate) struct Written {
    last: Option<(u64, UtcMicros)>,
    failing: bool,
}

impl Written {
    /// Whether `generation`, confirmed at `at`, replaces what is on disk: a newer generation always
    /// does, the same one once its persist time is [`RENEW`] old, an older one never.
    fn due(&self, generation: u64, at: UtcMicros) -> bool {
        let renew_us = i64::try_from(RENEW.as_micros()).unwrap_or(i64::MAX);
        self.last.is_none_or(|(written, written_at)| {
            generation > written || (generation == written && at.0 >= written_at.0.saturating_add(renew_us))
        })
    }
}

impl Runtime {
    /// Persists `snap`, confirmed against the store at `at`, as the node's local snapshot. Writes
    /// are serialized, so a slower write never replaces a newer generation.
    pub(crate) async fn persist_local(&self, snap: &Snapshot, at: UtcMicros) {
        let Some(cluster) = &self.cluster else { return };
        let Some(file) = &cluster.settings.snapshot else {
            return;
        };
        let mut written = cluster.written.lock().await;
        if !written.due(snap.generation, at) {
            return;
        }
        match file.write(&self.sealer, snap, at).await {
            Ok(()) => {
                written.last = Some((snap.generation, at));
                written.failing = false;
            }
            Err(e) => {
                if !written.failing {
                    tracing::warn!(error = %e, "the local snapshot could not be written");
                }
                written.failing = true;
            }
        }
    }

    /// Records that the serving generation was just read from the store, and persists it.
    pub async fn confirm_from_store(&self) {
        let Some(cluster) = &self.cluster else { return };
        let now = self.now();
        cluster.confirm(now);
        self.persist_local(&self.gens.load(), now).await;
    }
}

/// Renews the persist time of the serving generation every [`RENEW`] while the follower keeps
/// confirming it against the store; new generations are persisted when they are applied.
pub(crate) async fn run(rt: Arc<Runtime>) {
    let Some(cluster) = &rt.cluster else { return };
    while pause(&rt, cluster.settings.heartbeat_interval).await {
        if let Some(confirmed) = cluster.confirmed() {
            rt.persist_local(&rt.gens.load(), confirmed).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ConfigState;

    fn sealer() -> Sealer {
        Sealer::new(&[vec![7u8; 32]]).unwrap()
    }

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "antirion-local-{:x}",
            antirion_core::ResourceUid::random().0
        ))
    }

    #[tokio::test]
    async fn a_written_snapshot_reads_back_with_its_secrets_sealed() {
        let dir = temp_dir();
        let doc = serde_json::json!({
            "version": 1,
            "providers": {"p": {"type": "openai", "api_key": "sk-literal-secret-value"}},
        });
        let paths = vec![
            antirion_config::DocPath::root()
                .key("providers")
                .key("p")
                .key("api_key"),
        ];
        let state = ConfigState {
            generation: 4,
            doc,
            meta: Metadata::default(),
        };
        let pepper = antirion_identity::Pepper::new(1, vec![1u8; 32]);
        let snap =
            crate::snapshot::build(state, paths, &antirion_config::secrets::SystemSecrets, &pepper).unwrap();
        let file = SnapshotFile::new(&dir, &sealer(), Some(&[9u8; 32])).unwrap();
        file.write(&sealer(), &snap, UtcMicros(1234)).await.unwrap();

        let raw = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(!raw.contains("sk-literal-secret-value"));
        assert!(raw.contains(crate::atrest::PREFIX));

        let loaded = SnapshotFile::read(&dir, &sealer()).unwrap().unwrap();
        assert_eq!(loaded.persisted_at, UtcMicros(1234));
        assert_eq!(loaded.state.generation, 4);
        assert_eq!(
            loaded.state.doc.pointer("/providers/p/api_key"),
            Some(&Value::from("sk-literal-secret-value"))
        );
        assert_eq!(loaded.pepper, Some(vec![9u8; 32]));
        let other = Sealer::new(&[vec![8u8; 32]]).unwrap();
        assert!(SnapshotFile::read(&dir, &other).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_a_newer_generation_or_a_due_renewal_is_written() {
        let renew = i64::try_from(RENEW.as_micros()).unwrap();
        let mut w = Written::default();
        assert!(w.due(1, UtcMicros(0)));
        w.last = Some((3, UtcMicros(1_000)));
        assert!(w.due(4, UtcMicros(1_000)), "a new generation is written at once");
        assert!(
            !w.due(2, UtcMicros(1_000 + renew)),
            "an older generation never replaces a newer one"
        );
        assert!(!w.due(3, UtcMicros(1_000 + renew - 1)));
        assert!(w.due(3, UtcMicros(1_000 + renew)));
    }

    #[test]
    fn no_snapshot_is_none() {
        assert!(SnapshotFile::read(&temp_dir(), &sealer()).unwrap().is_none());
    }
}

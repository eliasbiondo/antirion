//! Starting a node: read and validate the startup document, open the store, build the first
//! generation, apply the document's dynamic keys, and serve until a stop signal.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use antirion_config::model::STATIC_KEYS;
use antirion_config::scalar::{parse_duration, parse_size};
use antirion_config::secrets::{SecretSource, SystemSecrets, expand_document};
use antirion_config::views::{StaticView, view};
use antirion_config::{ConfigError, Format};
use antirion_core::audit::{Actor, ActorKind};
use antirion_core::{RequestId, ResourceUid};
use antirion_identity::{Credential, Pepper, Plane};
use antirion_obs::{BuildInfo, Metrics};
use antirion_runtime::atrest::Sealer;
use antirion_runtime::changes::merge_apply;
use antirion_runtime::cluster::SnapshotFile;
use antirion_runtime::probes::Waiting;
use antirion_runtime::security::SecurityRt;
use antirion_runtime::spool::SpoolSettings;
use antirion_runtime::state::{ConfigState, Metadata};
use antirion_runtime::{
    AuditContext, Console, HttpTimeouts, Mutation, NodeInfo, Runtime, RuntimeParts, Statics, TestMode,
};
use antirion_store::{Store, StoreError};
use anyhow::{Context, anyhow, bail};
use serde_json::{Map, Value};

use crate::cluster::Opened;

/// What `serve` was asked to do.
#[derive(Debug)]
pub struct ServeOptions {
    pub config: Option<PathBuf>,
    pub test_mode: bool,
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const COMMIT: &str = env!("ANTIRION_COMMIT");
const RUSTC: &str = env!("ANTIRION_RUSTC");

/// Reads and fully validates a document; every error is returned with its path.
pub fn load_document(path: &Path) -> anyhow::Result<Value> {
    let bytes =
        std::fs::read(path).with_context(|| format!("cannot read the config file {}", path.display()))?;
    let format = Format::from_file_name(&path.to_string_lossy());
    let mut doc = antirion_config::parse(&bytes, format).map_err(|e| anyhow!(render_errors(&e)))?;
    let validation = antirion_config::validate(&mut doc, &Value::Null);
    if !validation.is_valid() {
        bail!(render_errors(&validation.errors));
    }
    expand_document(&doc, &Offline).map_err(|e| anyhow!(render_errors(&e)))?;
    Ok(doc)
}

/// The environment and files; the external stores are read when the node starts, so a document
/// checked offline takes their references as they are.
#[derive(Debug)]
struct Offline;

impl SecretSource for Offline {
    fn env(&self, name: &str) -> Option<String> {
        SystemSecrets.env(name)
    }

    fn file(&self, path: &str) -> std::io::Result<String> {
        SystemSecrets.file(path)
    }

    fn store(&self, _: &str, _: &str) -> Result<String, String> {
        Ok(String::new())
    }
}

/// One line per error: `path: code: message`.
#[must_use]
pub fn render_errors(errors: &[ConfigError]) -> String {
    let lines: Vec<String> = errors
        .iter()
        .map(|e| format!("{}: {}: {}", e.path, e.code.as_str(), e.message))
        .collect();
    format!("the configuration is invalid:\n{}", lines.join("\n"))
}

pub(crate) fn resolved_string(doc: &Value, pointer: &str) -> anyhow::Result<Option<String>> {
    match doc.pointer(pointer).and_then(Value::as_str) {
        None => Ok(None),
        Some(raw) => Ok(Some(
            antirion_config::secrets::expand(raw, &SystemSecrets)
                .map_err(|e| anyhow!(e.0))?
                .into_owned(),
        )),
    }
}

pub(crate) fn random_bytes(n: usize) -> anyhow::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).map_err(|e| anyhow!("no random source: {e}"))?;
    Ok(buf)
}

async fn resolve_addr(listen: &str) -> anyhow::Result<SocketAddr> {
    tokio::net::lookup_host(listen)
        .await
        .with_context(|| format!("cannot resolve the listen address {listen}"))?
        .next()
        .ok_or_else(|| anyhow!("the listen address {listen} resolves to nothing"))
}

/// The bootstrap credential: the configured one, or a generated token kept in the store directory.
fn bootstrap_token(doc: &Value, store_dir: &Path) -> anyhow::Result<String> {
    if let Some(t) = resolved_string(doc, "/auth/admin/bootstrap_token")? {
        return Ok(t);
    }
    let file = store_dir.join("bootstrap-token");
    if let Ok(existing) = std::fs::read_to_string(&file) {
        let token = existing.trim().to_owned();
        if !token.is_empty() {
            return Ok(token);
        }
    }
    let token = Credential::generate(Plane::Admin)
        .map_err(|e| anyhow!("no random source: {e}"))?
        .expose()
        .to_owned();
    write_private(&file, &token)?;
    tracing::warn!(path = %file.display(), "generated a bootstrap admin token; it is in this file");
    Ok(token)
}

fn write_private(path: &Path, content: &str) -> anyhow::Result<()> {
    write_private_bytes(path, content.as_bytes())
}

#[cfg(unix)]
fn write_private_bytes(path: &Path, content: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(content)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private_bytes(path: &Path, content: &[u8]) -> anyhow::Result<()> {
    Ok(std::fs::write(path, content)?)
}

fn statics(doc: &Value, v: &StaticView) -> Statics {
    let mut static_doc = Map::new();
    for key in STATIC_KEYS {
        if let Some(value) = doc.get(*key) {
            static_doc.insert((*key).to_owned(), value.clone());
        }
    }
    let api = &v.admin.api;
    Statics {
        doc: Value::Object(static_doc),
        default_page_size: api.default_page_size.unwrap_or(100),
        max_page_size: api.max_page_size.unwrap_or(1000),
        max_document: api
            .max_document
            .as_deref()
            .and_then(parse_size)
            .unwrap_or(10 << 20),
        max_wait: api
            .max_wait
            .as_deref()
            .and_then(parse_duration)
            .unwrap_or(Duration::from_secs(30)),
        include_diffs: v.admin.audit.include_diffs.unwrap_or(true),
        config_source: v.admin.config.source.clone().unwrap_or_else(|| "file".to_owned()),
        dns_servers: doc
            .pointer("/server/upstream/dns/servers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|s| s.parse().ok())
            .collect(),
        config_prune: v.admin.config.prune.unwrap_or(true),
        upstream_prior_knowledge: v.server.upstream.http2.as_deref() == Some("prior_knowledge"),
        max_connections_per_origin: v.server.upstream.max_connections_per_origin.unwrap_or(64),
        http: {
            let d = HttpTimeouts::default();
            let h = &v.server.http;
            let dur = |s: &Option<String>, default| s.as_deref().and_then(parse_duration).unwrap_or(default);
            HttpTimeouts {
                header_read: dur(&h.header_read_timeout, d.header_read),
                body_read: dur(&h.body_read_timeout, d.body_read),
                idle: dur(&h.idle_timeout, d.idle),
            }
        },
        ui: Console {
            enabled: v.admin.ui.enabled.unwrap_or(true),
            path: v
                .admin
                .ui
                .path
                .as_deref()
                .unwrap_or("/ui")
                .trim_end_matches('/')
                .to_owned(),
            data_plane_url: v.admin.ui.data_plane_url.clone(),
        },
    }
}

pub(crate) async fn persisted_state(store: &Store, sealer: &Sealer) -> anyhow::Result<ConfigState> {
    match store.load_config().await? {
        None => Ok(ConfigState::empty()),
        Some(stored) => {
            let mut doc: Value =
                serde_json::from_str(&stored.document).context("the stored configuration is corrupt")?;
            sealer.open_document(&mut doc)?;
            Ok(ConfigState {
                generation: stored.generation,
                doc,
                meta: serde_json::from_str::<Metadata>(&stored.metadata)
                    .context("the stored metadata is corrupt")?,
            })
        }
    }
}

/// The keys that encrypt stored secrets: `security.secrets.encryption_keys` of the startup file
/// (the first encrypts), then the key generated in the data directory, which is created when no key
/// is configured.
fn sealer(doc: &Value, store_dir: &Path) -> anyhow::Result<Sealer> {
    let mut keys = configured_keys(doc)?;
    let configured = keys.len();
    let file = store_dir.join("encryption.key");
    match std::fs::read(&file) {
        Ok(k) if k.len() == 32 => keys.push(k),
        Ok(_) => bail!("{} is not a 32-byte key", file.display()),
        Err(_) if keys.is_empty() => {
            let k = random_bytes(32)?;
            write_private_bytes(&file, &k)?;
            keys.push(k);
        }
        Err(_) => {}
    }
    Ok(Sealer::new(&keys)?.with_configured(configured))
}

/// `security.secrets.encryption_keys` of the startup file, decoded.
pub(crate) fn configured_keys(doc: &Value) -> anyhow::Result<Vec<Vec<u8>>> {
    use base64::Engine as _;
    let mut keys = Vec::new();
    let configured = doc
        .pointer("/security/secrets/encryption_keys")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    for (i, entry) in configured.enumerate() {
        let text = antirion_config::secrets::expand(entry, &SystemSecrets).map_err(|e| anyhow!(e.0))?;
        let key = base64::engine::general_purpose::STANDARD
            .decode(text.trim())
            .ok()
            .filter(|k| k.len() == 32)
            .ok_or_else(|| anyhow!("security.secrets.encryption_keys[{i}] must be the base64 of 32 bytes"))?;
        keys.push(key);
    }
    Ok(keys)
}

/// `store.ledger` and `store.spool`; the spool lives under the data directory by default.
fn spool_settings(v: &StaticView, store_dir: &Path) -> SpoolSettings {
    SpoolSettings {
        dir: v
            .store
            .spool
            .path
            .as_ref()
            .map_or_else(|| store_dir.join("spool"), PathBuf::from),
        max_bytes: v
            .store
            .spool
            .max_size
            .as_deref()
            .and_then(parse_size)
            .unwrap_or(1 << 30),
        reject_budgeted: v.store.spool.on_full.as_deref() != Some("drop"),
        flush_interval: v
            .store
            .ledger
            .flush_interval
            .as_deref()
            .and_then(parse_duration)
            .unwrap_or(Duration::from_secs(1)),
    }
}

/// A node value that holds a secret: created sealed when absent, opened when read.
pub(crate) async fn sealed_value(
    store: &Store,
    sealer: &Sealer,
    name: &str,
    init: Vec<u8>,
) -> anyhow::Result<Vec<u8>> {
    let location = format!("node_values/{name}");
    let sealed = sealer.seal(&location, &init)?.into_bytes();
    let stored = store.get_or_init(name, sealed).await?;
    Ok(sealer.open_value(&location, stored)?)
}

/// A configured node with its listeners bound.
#[derive(Debug)]
pub struct Node {
    pub rt: Arc<Runtime>,
    bound: antirion_server::Bound,
    tls: antirion_server::ListenerTls,
    threads: Arc<antirion_server::DataThreads>,
    store: Arc<Store>,
}

impl Node {
    #[must_use]
    pub fn data_addr(&self) -> Option<SocketAddr> {
        self.bound.data_addr()
    }

    #[must_use]
    pub fn admin_addr(&self) -> Option<SocketAddr> {
        self.bound.admin_addr()
    }

    /// Serves until the runtime's shutdown token is cancelled, then flushes what the node holds:
    /// ledger lines, logs, traces and metrics, within `flush_timeout`.
    pub async fn run(self) {
        let stop = self.rt.shutdown.clone();
        let threads = Arc::clone(&self.threads);
        antirion_server::serve(Arc::clone(&self.rt), self.bound, self.tls, threads, stop).await;
        self.rt.shutdown.cancel();
        self.rt.tasks.close();
        let flush = self.rt.drain.settings.flush_timeout;
        if tokio::time::timeout(flush, self.rt.tasks.wait()).await.is_err() {
            tracing::warn!("tasks were still running at exit");
        }
        self.threads.stop().await;
        self.store.close();
    }
}

/// Runs a node until a stop signal has drained it.
pub async fn serve(opts: ServeOptions) -> anyhow::Result<()> {
    let node = start(opts).await?;
    let signals = crate::lifecycle::Signals::new()?;
    node.rt
        .tasks
        .spawn(crate::lifecycle::run(Arc::clone(&node.rt), signals));
    node.run().await;
    Ok(())
}

/// Reads and applies the configuration, opens the store and binds the listeners.
pub async fn start(opts: ServeOptions) -> anyhow::Result<Node> {
    antirion_config::schema::self_check()
        .map_err(|e| anyhow!("the embedded config schema is broken: {e}"))?;
    let doc = match &opts.config {
        Some(path) => load_document(path)?,
        None => serde_json::json!({"version": 1}),
    };
    let static_view: StaticView = view(&doc).unwrap_or_default();
    let cluster = static_view.cluster.mode.as_deref() == Some("cluster");
    if cluster && opts.test_mode {
        bail!("test mode is refused in cluster mode");
    }
    if cluster {
        crate::cluster::check(&doc, &static_view)?;
    } else if let Some(r) = static_view.server.probes.readiness_requires.iter().find(|r| {
        r.as_str() != "store"
            && !(r.as_str() == "counters" && static_view.cluster.counters.backend.as_deref() == Some("redis"))
    }) {
        bail!("server.probes.readiness_requires: {r} is not a dependency of this node");
    }
    let store_dir = PathBuf::from(
        static_view
            .store
            .embedded
            .path
            .clone()
            .unwrap_or_else(|| "./antirion-data".to_owned()),
    );
    let data_addr = resolve_addr(static_view.server.listen.as_deref().unwrap_or("0.0.0.0:8080")).await?;
    let admin_addr = resolve_addr(static_view.admin.listen.as_deref().unwrap_or("127.0.0.1:8081")).await?;
    let bound = antirion_server::Bound::bind(data_addr, admin_addr).await?;
    let configured_pepper = match resolved_string(&doc, "/auth/key_pepper")? {
        Some(p) if p.len() >= 32 => Some(p.into_bytes()),
        Some(_) => bail!("auth.key_pepper must be at least 32 bytes"),
        None => None,
    };
    let mut waiting = None;
    let (store, sealer, node_id, state, stored_pepper, persisted_at) = if cluster {
        let node_id = static_view
            .cluster
            .node_id
            .clone()
            .unwrap_or_else(|| RequestId::generate().as_str().to_owned());
        let sealer = crate::cluster::sealer(&doc)?;
        let node = Waiting {
            node_id: node_id.clone(),
            version: VERSION.to_owned(),
        };
        let tls = match SecurityRt::compile(&doc) {
            Ok(security) => antirion_server::ListenerTls::for_security(&security).await?,
            Err(errors) => bail!(render_errors(&errors)),
        };
        let opening = Box::pin(crate::cluster::open(
            &doc,
            &static_view,
            &node_id,
            &store_dir,
            &sealer,
            configured_pepper.is_none(),
        ));
        let opened = bound.answer_while(&node, &tls, opening).await?;
        waiting = Some((node, tls));
        match opened {
            Opened::Store { store, state, pepper } => (store, sealer, node_id, state, pepper, None),
            Opened::Snapshot { store, local } => (
                store,
                sealer,
                node_id,
                local.state,
                local.pepper,
                Some(local.persisted_at),
            ),
        }
    } else {
        let store = Store::open_embedded(&store_dir).context("cannot open the embedded store")?;
        let sealer = sealer(&doc, &store_dir)?;
        let node_id_bytes = store
            .get_or_init(
                "node_id",
                format!("node-{}", &ResourceUid::random().to_hex()[..12]).into_bytes(),
            )
            .await?;
        let node_id = String::from_utf8_lossy(&node_id_bytes).into_owned();
        let pepper = match configured_pepper {
            Some(_) => None,
            None => Some(sealed_value(&store, &sealer, "key_pepper", random_bytes(32)?).await?),
        };
        let state = persisted_state(&store, &sealer).await?;
        (store, sealer, node_id, state, pepper, None)
    };
    let store = Arc::new(store);
    let started = antirion_core::Clock::now(&antirion_core::SystemClock).0;
    store
        .get_or_init(
            antirion_store::logs::CACHE_PREFIX_FROM,
            started.to_string().into_bytes(),
        )
        .await?;
    let (pepper_bytes, generated_pepper) = match (configured_pepper, stored_pepper) {
        (Some(p), _) => (p, false),
        (None, Some(p)) => (p, true),
        (None, None) => bail!("the generated key pepper could not be read"),
    };
    let mut cluster_settings = cluster.then(|| crate::cluster::settings(&static_view));
    if let Some(c) = cluster_settings.as_mut() {
        let pepper = generated_pepper.then_some(pepper_bytes.as_slice());
        c.snapshot = Some(SnapshotFile::new(&store_dir, &sealer, pepper)?);
    }
    let pepper = Pepper::new(1, pepper_bytes);
    let counters = if cluster || static_view.cluster.counters.backend.as_deref() == Some("redis") {
        Some(crate::cluster::counters(&doc, &static_view).await?)
    } else {
        None
    };
    let bootstrap = bootstrap_token(&doc, &store_dir)?;
    let secret_stores = Arc::new(antirion_runtime::secretstores::ExternalSecrets::new());
    antirion_runtime::secretstores::prefetch_at_startup(&secret_stores, &state.doc).await;
    let secrets: Arc<dyn SecretSource> = Arc::new(antirion_runtime::secretstores::Resolver::new(Arc::clone(
        &secret_stores,
    )));
    let secret_paths = antirion_config::schema::check_document(&state.doc).secret_paths;
    let first = antirion_runtime::snapshot::build(state, secret_paths, secrets.as_ref(), &pepper)
        .map_err(|e| anyhow!(render_errors(&e)))?;
    let test = opts.test_mode.then(|| Arc::new(TestMode::new()));
    let statics = statics(&doc, &static_view);
    let rt = Arc::new(Runtime::new(RuntimeParts {
        first,
        test: test.clone(),
        store: Arc::clone(&store),
        node: NodeInfo {
            id: node_id,
            version: VERSION.to_owned(),
            commit: COMMIT.to_owned(),
            test_hooks: cfg!(feature = "test-hooks"),
            started_at: antirion_core::Clock::now(&antirion_core::SystemClock),
        },
        statics,
        pepper,
        secrets,
        secret_stores,
        metrics: Metrics::new(&BuildInfo {
            version: VERSION.to_owned(),
            commit: COMMIT.to_owned(),
            rustc: RUSTC.to_owned(),
            semconv: antirion_obs::semconv().to_owned(),
            test_mode: opts.test_mode,
        }),
        bootstrap_token: Some(bootstrap),
        cursor_key: random_bytes(32)?,
        shutdown: antirion_runtime::drain::Settings::from_view(&static_view.server.shutdown),
        liveness_stall: static_view
            .server
            .probes
            .liveness_stall
            .as_deref()
            .and_then(parse_duration)
            .unwrap_or(Duration::from_secs(10)),
        sealer: sealer.clone(),
        counters,
        cluster: cluster_settings,
        spool: spool_settings(&static_view, &store_dir),
        readiness_requires: static_view.server.probes.readiness_requires.clone(),
    }));
    if let Some(logs) = crate::logstore::clickhouse(&doc)? {
        rt.state.request_logs.use_clickhouse(Arc::new(logs));
    }
    if cluster && let Some(url) = resolved_string(&doc, "/cluster/counters/redis/url")? {
        let topology = antirion_redis::Topology::parse(
            static_view
                .cluster
                .counters
                .redis
                .topology
                .as_deref()
                .unwrap_or("standalone"),
        );
        rt.state.cache.use_cluster_counters(&url, topology);
    }
    let probe_interval = crate::cluster::settings(&static_view).probe_interval;
    let base = dynamic_keys(&doc);
    let watched = static_view.admin.config.file.clone().map(PathBuf::from);
    let startup_doc = match &watched {
        Some(path) => crate::watch::compose(&base, path).await.map_err(|e| anyhow!(e))?,
        None => base.clone(),
    };
    match (persisted_at, &waiting) {
        (Some(at), _) => {
            rt.started_from_snapshot(at);
            rt.tasks.spawn(crate::deferred::apply_when_store_returns(
                Arc::clone(&rt),
                startup_doc,
                probe_interval,
            ));
        }
        (None, Some((node, tls))) => {
            let contact = Box::pin(crate::deferred::first_contact(&rt, &startup_doc, probe_interval));
            bound.answer_while(node, tls, contact).await?;
        }
        (None, None) => {
            rt.restore_disables().await;
            rt.restore_cache_epochs().await;
            rt.check_ledger()
                .await
                .map_err(|e| anyhow!("the spend ledger could not be read: {e}"))?;
            Box::pin(apply_document(&rt, &startup_doc, "startup"))
                .await
                .map_err(|e| anyhow!(e))?;
        }
    }
    let watchdog = Arc::clone(&rt);
    rt.tasks.spawn(async move {
        loop {
            watchdog.liveness.tick();
            tokio::select! {
                () = watchdog.shutdown.cancelled() => return,
                () = tokio::time::sleep(antirion_runtime::probes::Watchdog::TICK) => {}
            }
        }
    });
    let sweeper = Arc::clone(&rt);
    rt.tasks.spawn(async move {
        loop {
            tokio::select! {
                () = sweeper.shutdown.cancelled() => return,
                () = tokio::time::sleep(Duration::from_secs(5)) => sweeper.accounts.sweep(),
            }
        }
    });
    if !crate::deferred::load_mcp_key(&rt, &store, &sealer).await {
        rt.tasks.spawn(crate::deferred::retry_mcp_key(
            Arc::clone(&rt),
            Arc::clone(&store),
            sealer.clone(),
            probe_interval,
        ));
    }
    if let (Some(path), true) = (watched, static_view.admin.config.watch.unwrap_or(false)) {
        let debounce = static_view
            .admin
            .config
            .watch_debounce
            .as_deref()
            .and_then(parse_duration)
            .unwrap_or(Duration::from_millis(500));
        let watcher = crate::watch::Watcher {
            rt: Arc::clone(&rt),
            path,
            base,
            debounce,
        };
        rt.tasks.spawn(watcher.run());
    }

    rt.tasks
        .spawn(antirion_runtime::secretwatch::run(Arc::clone(&rt)));
    rt.tasks
        .spawn(antirion_runtime::secretstores::run(Arc::clone(&rt)));
    crate::hardening::report_unlocked_secrets();
    rt.tasks.spawn(Arc::clone(&rt).write_ledger());
    rt.tasks.spawn(Arc::clone(&rt).write_logs());
    rt.tasks.spawn(Arc::clone(&rt).sweep_playground_keys());
    rt.tasks.spawn(Arc::clone(&rt).sweep_rotations());
    rt.tasks.spawn(antirion_runtime::checks::run(Arc::clone(&rt)));
    rt.tasks.spawn(antirion_runtime::push::run(Arc::clone(&rt)));
    rt.tasks.spawn(antirion_runtime::pools::run(Arc::clone(&rt)));
    rt.tasks.spawn(antirion_runtime::bandits::run(Arc::clone(&rt)));
    rt.tasks
        .spawn(Arc::clone(&rt.admission).watch_memory(rt.shutdown.clone()));
    rt.tasks.spawn(antirion_runtime::wasm::clock(Arc::clone(&rt)));
    antirion_runtime::alerting::spawn(&rt);
    antirion_runtime::cluster::spawn(&rt, probe_interval);
    antirion_runtime::people::spawn(&rt);
    antirion_runtime::budgets::spawn(&rt);
    antirion_runtime::cache_console::spawn(&rt);
    antirion_runtime::operations::spawn(&rt);

    let tls = antirion_server::ListenerTls::load(&rt).await?;
    tracing::info!(data = ?bound.data_addr(), admin = ?bound.admin_addr(), "listening");
    if test.is_some() {
        let stop = rt.shutdown.clone();
        rt.tasks.spawn(async move {
            loop {
                tracing::warn!("test mode is on");
                tokio::select! {
                    () = stop.cancelled() => return,
                    () = tokio::time::sleep(Duration::from_secs(60)) => {}
                }
            }
        });
    }
    let threads = Arc::new(antirion_server::DataThreads::start(rt.tasks.clone())?);
    Ok(Node {
        rt,
        bound,
        tls,
        threads,
        store,
    })
}

/// The dynamic keys of a document.
pub(crate) fn dynamic_keys(doc: &Value) -> Map<String, Value> {
    let mut dynamic = doc.as_object().cloned().unwrap_or_default();
    for key in STATIC_KEYS {
        dynamic.remove(*key);
    }
    if let Some(admission) = doc.pointer("/server/admission") {
        dynamic.insert("server".to_owned(), serde_json::json!({"admission": admission}));
    }
    dynamic
}

/// Why a document could not be applied.
#[derive(Debug)]
pub(crate) enum ApplyError {
    /// The resulting configuration is invalid.
    Invalid(Vec<ConfigError>),
    /// The store could not take the new generation.
    Store(StoreError),
    /// The snapshot builder stopped.
    Builder,
}

impl ApplyError {
    /// Whether applying again can succeed: the store could not be reached, or another node
    /// committed first.
    pub(crate) fn retryable(&self) -> bool {
        matches!(self, ApplyError::Store(e) if e.is_outage() || matches!(e, StoreError::Conflict))
    }
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Invalid(errors) => f.write_str(&render_errors(errors)),
            ApplyError::Store(e) => write!(f, "the configuration could not be stored: {e}"),
            ApplyError::Builder => {
                f.write_str("the configuration could not be built on the stored generation")
            }
        }
    }
}

impl std::error::Error for ApplyError {}

/// Applies a document's dynamic keys as `admin.config.source`, with that source's prune setting.
pub(crate) async fn apply_document(
    rt: &Runtime,
    dynamic: &Map<String, Value>,
    actor: &str,
) -> Result<(), ApplyError> {
    let source = rt.statics.config_source.clone();
    let prune = rt.statics.config_prune;
    let audit = AuditContext {
        actor: Actor {
            kind: ActorKind::System,
            id: Some(actor.to_owned()),
        },
        action: "config.apply".to_owned(),
        resource: "config".to_owned(),
        request_id: RequestId::generate().as_str().to_owned(),
        source_ip: None,
        status_code: 200,
        reason: None,
    };
    let result = rt
        .mutate(|state| {
            let plan = merge_apply(state, dynamic, &source, prune, &Value::Null);
            Ok::<_, std::convert::Infallible>(Mutation {
                candidate: plan.candidate,
                writer: format!("apply:{source}"),
                written: plan.written,
                credentials: Vec::new(),
                meta_edits: Vec::new(),
                dry_run: false,
                source: source.clone(),
                audit: Some(audit),
            })
        })
        .await;
    match result {
        Ok(_) => Ok(()),
        Err(antirion_runtime::CommitError::Invalid(errors)) => Err(ApplyError::Invalid(errors)),
        Err(antirion_runtime::CommitError::Store(e)) => Err(ApplyError::Store(e)),
        Err(antirion_runtime::CommitError::Builder | antirion_runtime::CommitError::Rejected(_)) => {
            Err(ApplyError::Builder)
        }
    }
}

#[cfg(test)]
mod tests {
    use antirion_config::{DetailCode, DocPath};

    use super::*;

    #[test]
    fn a_store_that_cannot_take_the_document_is_not_an_invalid_configuration() {
        let down = ApplyError::Store(StoreError::Unavailable);
        assert!(down.retryable());
        assert!(!down.to_string().contains("invalid"), "{down}");
        assert!(!down.to_string().contains("malformed_document"), "{down}");
        assert!(ApplyError::Store(StoreError::Timeout).retryable());
        assert!(ApplyError::Store(StoreError::Conflict).retryable());
        assert!(!ApplyError::Store(StoreError::Config("refused".into())).retryable());
        assert!(!ApplyError::Builder.retryable());
        let invalid = ApplyError::Invalid(vec![ConfigError::new(
            &DocPath::root(),
            DetailCode::MalformedDocument,
            "bad".to_owned(),
        )]);
        assert!(!invalid.retryable());
        assert!(
            invalid.to_string().starts_with("the configuration is invalid:"),
            "{invalid}"
        );
    }
}

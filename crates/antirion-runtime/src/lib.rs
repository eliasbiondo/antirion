//! The running gateway: config generations and their snapshots, runtime state that survives reloads,
//! the clock, test mode, the store and the node's static settings.

#![forbid(unsafe_code)]

pub mod access;
pub mod admin_auth;
pub mod admission;
pub mod alerting;
pub mod atrest;
pub mod bandits;
pub mod budgets;
pub mod cache_console;
pub mod changes;
pub mod checks;
pub mod client_ip;
pub mod cluster;
pub mod commit;
mod console;
pub mod drain;
pub mod extstats;
mod fileonly;
pub mod geoip;
pub mod guardtest;
pub mod header_rules;
mod health;
pub mod history;
pub mod identity;
pub mod jwt;
pub mod keypolicy;
mod ledger;
mod logins;
pub mod operations;
pub mod operators;
mod otlp_metrics;
pub mod people;
pub mod playground;
pub mod pools;
pub mod pressure;
pub mod pricing;
pub mod probes;
pub mod push;
mod recost;
pub mod replay;
pub mod reqlog;
pub mod respcache;
pub mod responses;
pub mod rotation;
pub mod safety;
pub mod saml;
pub mod secretstores;
pub mod secretwatch;
pub mod security;
pub mod snapshot;
pub mod spool;
pub mod state;
pub mod telemetry;
pub mod testmode;
pub mod usage;
pub mod wasm;
pub mod wording;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use antirion_config::secrets::SecretSource;
use antirion_core::{Clock, RequestId, SplitMix64, UtcMicros};
use antirion_egress::EgressClient;
use antirion_identity::Pepper;
use antirion_limits::spend::Accounts;
use antirion_obs::{Fam, Metrics};
use antirion_resilience::HealthRegistry;
use antirion_router::{AffinityStore, RoutingState};
use antirion_store::Store;
use arc_swap::ArcSwap;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

pub use commit::{AuditContext, CommitError, Committed, MetaEdit, Mutation};
pub use console::Console;
pub use snapshot::Snapshot;
pub use state::ConfigState;
pub use testmode::TestMode;

/// The current generation's snapshot, swapped atomically. In-flight requests keep the one they
/// loaded.
#[derive(Debug)]
pub struct Generations {
    current: ArcSwap<Snapshot>,
}

impl Generations {
    #[must_use]
    pub fn new(first: Snapshot) -> Generations {
        Generations {
            current: ArcSwap::from_pointee(first),
        }
    }

    #[must_use]
    pub fn load(&self) -> Arc<Snapshot> {
        self.current.load_full()
    }

    pub(crate) fn swap(&self, next: Arc<Snapshot>) {
        self.current.store(next);
    }
}

/// State that must survive config generations, keyed by stable identities.
#[derive(Debug, Default)]
pub struct RuntimeState {
    /// Selection state: SWRR counters, latency and success estimates, in-flight attempts.
    pub routing: Arc<RoutingState>,
    /// Breakers, cooldowns, key state and manual disables.
    pub health: HealthRegistry,
    pub affinity: AffinityStore,
    pub key_usage: usage::KeyUsage,
    pub verified: usage::Verifications,
    pub request_logs: reqlog::RequestLogs,
    pub cache: antirion_cache::Cache,
    pub mcp: Arc<antirion_mcp::McpState>,
    pub telemetry: telemetry::Telemetry,
    /// Stored Responses and native response-id links.
    pub responses: responses::ResponsesStore,
    /// Single sign-on logins started and not yet completed, by their `state`.
    pub oidc_logins: parking_lot::Mutex<HashMap<String, OidcLogin>>,
    /// Extension hook calls on this node.
    pub extensions: extstats::ExtensionStats,
    /// The console's sign-in throttles, mail queue and login-page ticker.
    pub console: operators::ConsoleState,
    /// The way into the data plane and the keys the playground minted.
    pub playground: playground::PlaygroundState,
    /// Since when the node has been ready, while it is.
    pub ready_since: parking_lot::Mutex<Option<UtcMicros>>,
    /// Alert delivery queues, rotation overlaps and the conditions rules wait on.
    pub alerting: alerting::Hub,
    /// Reasoning that translated responses attached to tool calls.
    pub replay: replay::ReplayStore,
    /// The failed-authentication throttle and the external authorization hook's answers.
    pub access: access::AccessState,
    /// The guardrail test endpoint's way into the data plane.
    pub guard_tests: guardtest::GuardrailTests,
    /// The rate-limit headers each provider credential's answers carried last.
    pub headroom: antirion_providers::headroom::Headroom,
    /// The key reminders this node has sent or seen claimed.
    pub key_reminders: people::reminders::Claimed,
    /// Key anomaly detection: what keys did since the last evaluation, and their history.
    pub anomalies: people::anomalies::Detector,
    /// Budget events waiting to be written and the forecast alerts this node raised.
    pub budgets: budgets::BudgetsState,
    /// Safety events and guardrail counts waiting to be written and announced.
    pub safety: safety::SafetyEvents,
    /// Semantic cache tallies not yet stored and the schedule's last minute.
    pub cache_console: cache_console::CacheConsoleState,
    /// Ingress pauses and the erased tenants this node knows of.
    pub operations: operations::Operations,
    /// The identity providers' metadata parsed for SAML sign-ins.
    pub saml: saml::SamlState,
}

/// A single sign-on login waiting for its callback.
#[derive(Clone, Debug)]
pub struct OidcLogin {
    pub provider: String,
    pub nonce: String,
    pub verifier: String,
    pub redirect_uri: String,
    pub return_to: String,
    pub started: UtcMicros,
    /// A test sign-in: the callback reports what it would do and starts no session.
    pub test: bool,
    /// The sign-in asked to trust the device; honoured only when the provider asserts a second factor.
    pub trust_device: bool,
}

/// Node facts reported by probes and the status endpoint.
#[derive(Clone, Debug)]
pub struct NodeInfo {
    pub id: String,
    pub version: String,
    pub commit: String,
    /// The build has the test hooks compiled in; they act only in test mode.
    pub test_hooks: bool,
    /// When the process started (the system clock).
    pub started_at: UtcMicros,
}

/// Static settings read once at startup.
#[derive(Clone, Debug)]
pub struct Statics {
    /// The static keys of the startup document, as read (served by `/settings/<key>`).
    pub doc: Value,
    pub default_page_size: usize,
    pub max_page_size: usize,
    pub max_document: u64,
    pub max_wait: Duration,
    pub include_diffs: bool,
    pub config_source: String,
    pub config_prune: bool,
    /// `server.upstream.dns.servers`: when set, every egress lookup asks these servers.
    pub dns_servers: Vec<std::net::SocketAddr>,
    /// `server.upstream.http2: prior_knowledge`: plain `http` upstreams are spoken to in HTTP/2.
    pub upstream_prior_knowledge: bool,
    /// `server.upstream.max_connections_per_origin`.
    pub max_connections_per_origin: usize,
    pub http: HttpTimeouts,
    pub ui: Console,
}

/// `server.http`: how long a client may take, on the data listener.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct HttpTimeouts {
    /// To a complete request head, from accept or from a request's first byte.
    pub header_read: Duration,
    /// The longest gap between request-body bytes.
    pub body_read: Duration,
    /// A connection without a request in progress is closed after this long.
    pub idle: Duration,
}

impl Default for HttpTimeouts {
    fn default() -> HttpTimeouts {
        HttpTimeouts {
            header_read: Duration::from_secs(10),
            body_read: Duration::from_secs(30),
            idle: Duration::from_secs(620),
        }
    }
}

/// An in-flight request; dropping it ends the count, also when the request's task unwinds.
#[derive(Debug)]
pub struct InFlight<'a>(&'a AtomicU64);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Everything a running node shares between its planes.
#[derive(Debug)]
pub struct Runtime {
    pub gens: Generations,
    pub state: RuntimeState,
    /// Limit and budget counters, reservations and the spend ledger.
    pub accounts: Accounts,
    pub clock: Arc<dyn Clock>,
    pub test: Option<Arc<TestMode>>,
    pub store: Arc<Store>,
    pub node: NodeInfo,
    pub statics: Statics,
    pub pepper: Pepper,
    pub secrets: Arc<dyn SecretSource>,
    /// Values read from the external secret stores, which `secrets` resolves from.
    pub secret_stores: Arc<secretstores::ExternalSecrets>,
    pub metrics: Metrics,
    pub egress: EgressClient,
    /// Upstream credentials that must be acquired (tokens, AWS chain credentials).
    pub creds: Arc<antirion_providers::Credentials>,
    /// The key sets of the JWT providers.
    pub jwks: jwt::JwksCache,
    /// Every request and background task; shutdown waits for it.
    pub tasks: TaskTracker,
    pub shutdown: CancellationToken,
    /// Key for the integrity tag of list cursors.
    pub cursor_key: Vec<u8>,
    /// Inference requests holding or waiting for an admission slot.
    pub admission: Arc<admission::Admission>,
    pub drain: Arc<drain::Drain>,
    pub liveness: probes::Watchdog,
    /// Encrypts the secret values the node stores.
    pub sealer: atrest::Sealer,
    /// Shared-state machinery, in cluster mode.
    pub cluster: Option<cluster::Cluster>,
    /// Settled ledger lines on their way to the store.
    pub ledger: Arc<spool::LedgerWriter>,
    /// `server.probes.readiness_requires`.
    pub readiness_requires: Vec<String>,
    pub(crate) bootstrap_verifier: Option<String>,
    in_flight: AtomicU64,
    write_lock: tokio::sync::Mutex<()>,
}

/// What a runtime is built from.
#[derive(Debug)]
pub struct RuntimeParts {
    pub first: Snapshot,
    pub test: Option<Arc<TestMode>>,
    pub store: Arc<Store>,
    pub node: NodeInfo,
    pub statics: Statics,
    pub pepper: Pepper,
    pub secrets: Arc<dyn SecretSource>,
    pub secret_stores: Arc<secretstores::ExternalSecrets>,
    pub metrics: Metrics,
    pub bootstrap_token: Option<String>,
    pub cursor_key: Vec<u8>,
    pub shutdown: drain::Settings,
    pub liveness_stall: Duration,
    pub sealer: atrest::Sealer,
    /// The shared counter store and its command timeout, in cluster mode.
    pub counters: Option<(antirion_limits::store::valkey::ValkeyCounters, Duration)>,
    pub cluster: Option<cluster::ClusterSettings>,
    pub spool: spool::SpoolSettings,
    pub readiness_requires: Vec<String>,
}

impl Runtime {
    #[must_use]
    pub fn new(parts: RuntimeParts) -> Runtime {
        let clock: Arc<dyn Clock> = match &parts.test {
            Some(t) => t.clock.clone(),
            None => Arc::new(antirion_core::SystemClock),
        };
        let bootstrap_verifier = parts.bootstrap_token.as_deref().map(|t| parts.pepper.verifier(t));
        let resolver = if parts.statics.dns_servers.is_empty() {
            antirion_egress::resolve::Resolver::System
        } else {
            antirion_egress::resolve::Resolver::servers(&parts.statics.dns_servers).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "the configured DNS servers cannot be used; using the system resolver");
                antirion_egress::resolve::Resolver::System
            })
        };
        let mut egress = EgressClient::new(Duration::from_secs(10))
            .with_resolver(resolver)
            .with_h2c_connections(parts.statics.max_connections_per_origin);
        if parts.statics.upstream_prior_knowledge {
            egress = egress.with_prior_knowledge(parts.statics.max_connections_per_origin);
        }
        let tasks = TaskTracker::new();
        let creds = Arc::new(antirion_providers::Credentials::new(
            egress.clone(),
            Arc::clone(&clock),
            tasks.clone(),
        ));
        let admission = admission::Admission::new(parts.metrics.clone());
        let rt = Runtime {
            gens: Generations::new(parts.first),
            state: RuntimeState::default(),
            accounts: match parts.counters {
                Some((valkey, timeout)) => Accounts::with_counters(
                    Arc::clone(&clock),
                    &parts.node.id,
                    antirion_limits::store::Counters::Valkey(valkey),
                    timeout,
                ),
                None => Accounts::new(Arc::clone(&clock), &parts.node.id),
            },
            clock,
            test: parts.test,
            store: parts.store,
            node: parts.node,
            statics: parts.statics,
            pepper: parts.pepper,
            secrets: parts.secrets,
            secret_stores: parts.secret_stores,
            metrics: parts.metrics,
            egress,
            creds,
            jwks: jwt::JwksCache::default(),
            tasks,
            shutdown: CancellationToken::new(),
            cursor_key: parts.cursor_key,
            admission,
            drain: Arc::new(drain::Drain::new(parts.shutdown)),
            liveness: probes::Watchdog::new(parts.liveness_stall),
            sealer: parts.sealer,
            cluster: parts.cluster.map(cluster::Cluster::new),
            ledger: Arc::new(spool::LedgerWriter::new(parts.spool)),
            readiness_requires: parts.readiness_requires,
            bootstrap_verifier,
            in_flight: AtomicU64::new(0),
            write_lock: tokio::sync::Mutex::new(()),
        };
        rt.accounts.set_history(Arc::new(ledger::StoreHistory {
            store: Arc::clone(&rt.store),
            writer: Arc::clone(&rt.ledger),
        }));
        rt.observe_generation(&rt.gens.load());
        rt
    }

    /// Brings metrics limits, the generation gauge and the sink and exporter workers in line with
    /// a generation that was just swapped in.
    pub(crate) fn observe_generation(&self, snap: &Snapshot) {
        self.admission
            .configure(admission::Limits::effective(&snap.config.doc, &self.statics.doc));
        self.metrics.configure(&snap.obs.metrics.config);
        self.metrics
            .set(Fam::ConfigGeneration, &[], snap.generation as f64);
        self.state.cache.configure(&snap.cache);
        self.state
            .routing
            .canaries
            .applied(snap.routing.canary_keys(), self.now());
        self.accounts
            .engine()
            .set_lease_ttl(Duration::from_micros(snap.limits.limits.lease_ttl_us));
        let env = telemetry::Env {
            client: self.egress.clone(),
            policy: Arc::clone(&snap.security.egress),
            tls: snap.security.egress_tls.clone(),
            metrics: self.metrics.clone(),
            version: self.node.version.clone(),
            clock: Arc::clone(&self.clock),
        };
        self.state
            .telemetry
            .reconcile(snap, &env, &self.tasks, &self.shutdown);
    }

    /// Counts a config change attempted through `source` (`api`, `apply:<name>` or `file`).
    pub(crate) fn config_applied(&self, source: &str, applied: bool) {
        let source = if source.starts_with("apply") {
            "apply"
        } else if source.starts_with("file") {
            "file"
        } else {
            "api"
        };
        let result = if applied { "applied" } else { "rejected" };
        self.metrics.add(Fam::ConfigApplies, &[source, result], 1.0);
        if applied {
            let now = self.clock.now();
            self.metrics.set(Fam::ConfigLastApplied, &[], now.0 as f64 / 1e6);
        }
    }

    /// `config.applied` (`Ok` with the new generation) or `config.rejected` (`Err` with the error
    /// count) to the alert webhooks of `snap`.
    pub(crate) fn config_event(
        &self,
        snap: &Snapshot,
        source: &str,
        actor: Option<&str>,
        outcome: Result<u64, usize>,
    ) {
        let (kind, data) = match outcome {
            Ok(generation) => (
                "config.applied",
                serde_json::json!({"generation": generation, "source": source, "actor": actor}),
            ),
            Err(error_count) => (
                "config.rejected",
                serde_json::json!({"source": source, "actor": actor, "error_count": error_count}),
            ),
        };
        self.send_alert(snap, None, kind, data);
    }

    /// Sends a `deployment.state_changed` event for every deployment state change since the last
    /// call.
    pub fn publish_health_changes(&self, snap: &Snapshot) {
        for c in self.state.health.take_changes() {
            let provider = snap
                .providers
                .deployments
                .get(&c.deployment)
                .map(|d| d.provider.name.clone());
            self.send_alert(
                snap,
                None,
                "deployment.state_changed",
                serde_json::json!({
                    "provider": provider,
                    "deployment": c.deployment,
                    "from": c.from,
                    "to": c.to,
                    "reason": c.reason,
                }),
            );
        }
    }

    #[must_use]
    pub fn now(&self) -> UtcMicros {
        self.clock.now()
    }

    /// The egress of MCP servers and agents for request `request_id`, under `snap`'s egress policy and
    /// TLS profile; `None` when the snapshot has no egress TLS profile.
    #[must_use]
    pub fn mcp_egress(&self, snap: &Snapshot, request_id: &RequestId) -> Option<antirion_mcp::Egress> {
        Some(antirion_mcp::Egress {
            client: self.egress.clone(),
            policy: Arc::clone(&snap.security.egress),
            tls: snap.security.egress_tls.clone()?,
            request_id: request_id.clone(),
            subject: None,
        })
    }

    /// Runs `f` with this request's random stream: the seeded one in test mode after a seed was set,
    /// otherwise a fresh one from the thread's fast stream.
    pub fn with_rng<T>(&self, f: impl FnOnce(&mut SplitMix64) -> T) -> T {
        if let Some(t) = &self.test {
            let mut seeded = t.seeded();
            if let Some(rng) = seeded.as_mut() {
                return f(rng);
            }
        }
        f(&mut SplitMix64::from_thread())
    }

    /// Counts one inference request as in flight until the guard is dropped.
    #[must_use]
    pub fn in_flight_guard(&self) -> InFlight<'_> {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        InFlight(&self.in_flight)
    }

    #[must_use]
    pub fn in_flight(&self) -> u64 {
        self.in_flight.load(Ordering::Relaxed)
    }

    /// The degraded components of this node for `x-antirion-degraded`: `store` while the store is
    /// unreachable or ledger lines wait in the spool, `counters` while the counter store is.
    #[must_use]
    pub fn degraded(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.store.is_down() || self.ledger.spooling() {
            v.push("store");
        }
        if self.accounts.engine().is_store_down() {
            v.push("counters");
        }
        v
    }

    /// The shared store holding keys is unreachable, or has not been read since the node started.
    #[must_use]
    pub fn identity_store_down(&self) -> bool {
        self.cluster.is_some() && self.store.is_down()
    }

    /// `cluster` or `standalone`.
    #[must_use]
    pub fn mode(&self) -> &'static str {
        if self.cluster.is_some() {
            "cluster"
        } else {
            "standalone"
        }
    }

    /// Writes the ledger every flush interval until the node stops, then once more.
    pub async fn write_ledger(self: Arc<Self>) {
        loop {
            let stop = tokio::select! {
                () = self.shutdown.cancelled() => true,
                () = tokio::time::sleep(self.ledger.flush_interval()) => false,
            };
            self.flush_ledger().await;
            if stop {
                return;
            }
        }
    }

    /// Sends an alert event to the webhooks of `snap` subscribed to `kind` (only `sink` when set),
    /// and to the event rules that fire on it. It only queues work.
    pub fn send_alert(&self, snap: &Snapshot, sink: Option<&str>, kind: &str, data: Value) {
        self.emit(snap, sink, kind, &data, &[]);
    }

    /// Clears runtime state for the named scopes (all when empty), without touching config.
    pub async fn reset(&self, scopes: &[String]) -> Result<(), antirion_store::StoreError> {
        let all = scopes.is_empty();
        let has = |s: &str| all || scopes.iter().any(|x| x == s);
        if has("counters") {
            self.state.console.reset();
            self.state.access.reset();
            self.state.key_usage.clear();
            self.store.clear_use_marks().await?;
            self.state.anomalies.clear();
            self.store.clear_anomalies().await?;
        }
        if has("counters") || has("budgets") {
            self.accounts.clear().await;
            self.ledger.clear().await;
            self.store.clear_ledger().await?;
            self.state.budgets.clear();
            self.store.clear_budget_console().await?;
        }
        if has("health") {
            self.state.headroom.clear();
            self.store.clear_pressure().await?;
            self.state.routing.reset();
            self.store.clear_bandits().await?;
            self.state.health.clear();
            self.persist_disables().await;
        }
        if has("affinity") {
            self.state.affinity.clear();
            self.state.responses.clear_links();
            self.state.replay.clear();
            self.store.clear_objects().await?;
        }
        if has("cache") {
            self.state.cache.clear().await;
            self.state.cache_console.clear();
            self.store.clear_cache_console().await?;
            self.persist_cache_epochs().await;
            self.state.mcp.clear_caches();
        }
        if has("sessions") {
            self.state.mcp.clear_sessions();
        }
        if has("logs") {
            self.clear_logs().await?;
        }
        if has("sessions") || has("cache") {
            self.state.responses.clear();
        }
        if all && let Some(t) = &self.test {
            t.clear_seed();
        }
        if has("audit") {
            self.store.clear_audit().await?;
            self.store.clear_requests().await?;
            self.state.key_reminders.clear();
        }
        if has("alerts") {
            self.state.alerting.reset();
            self.store.clear_alerting().await?;
        }
        if has("audit") {
            self.state.operations.clear();
        }
        Ok(())
    }
}

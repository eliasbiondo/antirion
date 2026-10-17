//! External secret stores (`security.secrets.stores`): the values behind `${vault:…}`,
//! `${aws-sm:…}`, `${gcp-sm:…}` and `${azure-kv:…}`. Values are fetched before a document is
//! validated or built (resolution itself is synchronous and reads only this cache), kept for at
//! most their store's `ttl`, and fetched again in the background; a changed value rebuilds the
//! serving snapshot without a new generation. A failed refresh keeps the last value and raises
//! the built-in alert `secret_store_unavailable`. Each node resolves on its own.

mod cloud;
mod fetch;
mod settings;
mod vault;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use antirion_config::secrets::{SecretSource, SystemSecrets, store_references};
use antirion_core::{Secret, UtcMicros};
use antirion_egress::EgressClient;
use futures_util::future::join_all;
use parking_lot::Mutex;
use serde_json::Value;

pub(crate) use self::fetch::Cx;
use self::fetch::FetchError;
pub use self::settings::{API_TOKEN_FILES, check_api_write};
use self::settings::{Kind, StoreSettings, compile, store_of};
use crate::Runtime;

/// The built-in alert a failing store raises.
pub const ALERT_RULE: &str = "secret_store_unavailable";
/// How often values are checked against their ttl.
const POLL: Duration = Duration::from_secs(1);
/// How often a store that keeps failing raises its alert again.
const REALERT: Duration = Duration::from_secs(60);

/// A value read from a store, with the lease the store gave it.
pub(crate) struct Fetched {
    pub(crate) value: String,
    pub(crate) lease: Option<Duration>,
}

/// A JSON scalar as the text a secret field holds: strings as they are, numbers and booleans as
/// written; anything else is not a secret value.
pub(crate) fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// One reference's cached value.
struct Entry {
    store: &'static str,
    fingerprint: u64,
    value: Option<Arc<Secret>>,
    fetched: Option<Instant>,
    ttl: Duration,
    error: Option<String>,
}

impl Entry {
    fn due(&self, s: &StoreSettings, now: Instant) -> bool {
        self.fingerprint != s.fingerprint
            || self.value.is_none()
            || self
                .fetched
                .is_none_or(|f| now.saturating_duration_since(f) >= self.ttl)
    }
}

/// What a store's references last did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreStatus {
    /// References of the serving configuration that name the store.
    pub references: usize,
    /// When a value was last read from it (gateway clock).
    pub last_refresh_at: Option<UtcMicros>,
    /// Why the last failed read failed, while a reference is failing.
    pub last_error: Option<String>,
    pub failing: bool,
    alerted: Option<Instant>,
}

/// What one refresh changed.
#[derive(Debug, Default)]
struct Refreshed {
    changed: bool,
    raise: Vec<(&'static str, String)>,
    recovered: Vec<&'static str>,
}

/// The node's cache of values read from external stores.
#[derive(Default)]
pub struct ExternalSecrets {
    entries: Mutex<HashMap<String, Entry>>,
    status: Mutex<BTreeMap<&'static str, StoreStatus>>,
    vault_login: vault::Login,
    flight: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for ExternalSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalSecrets")
            .field("entries", &self.entries.lock().len())
            .finish_non_exhaustive()
    }
}

fn key(store: &str, reference: &str) -> String {
    format!("{store}:{reference}")
}

async fn read(
    cx: &Cx,
    s: &StoreSettings,
    login: &vault::Login,
    reference: &str,
) -> Result<Fetched, FetchError> {
    match &s.kind {
        Kind::Vault(v) => vault::fetch(cx, s, v, login, reference).await,
        Kind::Aws(a) => cloud::aws(cx, s, a, reference).await,
        Kind::Gcp(g) => cloud::gcp(cx, s, g, reference).await,
        Kind::Azure(a) => cloud::azure(cx, s, a, reference).await,
    }
}

impl ExternalSecrets {
    #[must_use]
    pub fn new() -> ExternalSecrets {
        ExternalSecrets::default()
    }

    /// The value of `${<store>:<reference>}` as last read, or why there is none.
    fn lookup(&self, store: &str, reference: &str) -> Result<String, String> {
        match self.entries.lock().get(&key(store, reference)) {
            Some(Entry { value: Some(v), .. }) => Ok(v.expose().to_owned()),
            Some(Entry { error: Some(e), .. }) => Err(e.clone()),
            _ => Err("the value has not been read from the store yet".to_owned()),
        }
    }

    /// Reads, for `doc`'s settings, every reference of `doc` that has no value for those settings
    /// or is due; returns what changed.
    async fn read_due(&self, cx: &Cx, doc: &Value, now: UtcMicros, only_missing: bool) -> Refreshed {
        let _flight = self.flight.lock().await;
        let stores = compile(doc, &SystemSecrets);
        let refs = store_references(doc);
        let started = Instant::now();
        let mut work = Vec::new();
        let mut out = Refreshed::default();
        {
            let mut entries = self.entries.lock();
            for (scheme, reference) in &refs {
                let k = key(scheme, reference);
                let configured = store_of(scheme).and_then(|name| stores.iter().find(|s| s.name == name));
                let Some(s) = configured else {
                    entries.insert(
                        k,
                        Entry {
                            store: store_of(scheme).unwrap_or("unknown"),
                            fingerprint: 0,
                            value: None,
                            fetched: None,
                            ttl: Duration::ZERO,
                            error: Some("no secret store of that name is configured".to_owned()),
                        },
                    );
                    continue;
                };
                let due = entries.get(&k).is_none_or(|e| {
                    if only_missing {
                        e.fingerprint != s.fingerprint || e.value.is_none()
                    } else {
                        e.due(s, started)
                    }
                });
                if due {
                    work.push((k, Arc::clone(s), reference.clone()));
                }
            }
        }
        let results = join_all(
            work.iter()
                .map(|(_, s, reference)| read(cx, s, &self.vault_login, reference)),
        )
        .await;
        let mut entries = self.entries.lock();
        let mut failing: BTreeMap<&'static str, String> = BTreeMap::new();
        let mut succeeded: Vec<&'static str> = Vec::new();
        for ((k, s, _), result) in work.iter().zip(results) {
            let previous = entries.remove(k).filter(|e| e.fingerprint == s.fingerprint);
            let entry = match result {
                Ok(f) => {
                    succeeded.push(s.name);
                    let same = previous
                        .as_ref()
                        .and_then(|e| e.value.as_ref())
                        .is_some_and(|v| v.expose() == f.value);
                    out.changed |= !same;
                    Entry {
                        store: s.name,
                        fingerprint: s.fingerprint,
                        value: Some(Arc::new(Secret::new(f.value))),
                        fetched: Some(Instant::now()),
                        ttl: f.lease.map_or(s.ttl, |l| l.min(s.ttl)),
                        error: None,
                    }
                }
                Err(e) => {
                    failing.insert(s.name, e.0.clone());
                    match previous {
                        Some(mut kept) if kept.value.is_some() => {
                            kept.fetched = Some(Instant::now());
                            kept.error = Some(e.0);
                            kept
                        }
                        _ => Entry {
                            store: s.name,
                            fingerprint: s.fingerprint,
                            value: None,
                            fetched: None,
                            ttl: s.ttl,
                            error: Some(e.0),
                        },
                    }
                }
            };
            entries.insert(k.clone(), entry);
        }
        if only_missing {
            return out;
        }
        let wanted: Vec<String> = refs.iter().map(|(s, r)| key(s, r)).collect();
        entries.retain(|k, _| wanted.contains(k));
        let mut status = self.status.lock();
        status.retain(|name, st| {
            let kept = stores.iter().any(|s| s.name == *name);
            if !kept && st.failing {
                out.recovered.push(name);
            }
            kept
        });
        for s in &stores {
            let st = status.entry(s.name).or_default();
            st.references = entries.values().filter(|e| e.store == s.name).count();
            if succeeded.contains(&s.name) {
                st.last_refresh_at = Some(now);
            }
            let still_failing = entries.values().any(|e| e.store == s.name && e.error.is_some());
            match (still_failing, failing.get(s.name)) {
                (true, why) => {
                    if let Some(why) = why {
                        st.last_error = Some(why.clone());
                    }
                    st.failing = true;
                    if st
                        .alerted
                        .is_none_or(|a| started.saturating_duration_since(a) >= REALERT)
                    {
                        st.alerted = Some(started);
                        out.raise
                            .push((s.name, st.last_error.clone().unwrap_or_default()));
                    }
                }
                (false, _) => {
                    if st.failing {
                        out.recovered.push(s.name);
                    }
                    st.failing = false;
                    st.last_error = None;
                    st.alerted = None;
                }
            }
        }
        out
    }

    /// Reads what `doc` references and this cache has no value for under `doc`'s store settings.
    /// Run before a document is validated or built.
    pub(crate) async fn prefetch(&self, cx: &Cx, doc: &Value, now: UtcMicros) {
        if store_references(doc).is_empty() {
            return;
        }
        self.read_due(cx, doc, now, true).await;
    }

    /// Each configured store with what its references last did.
    #[must_use]
    pub fn status(&self) -> BTreeMap<&'static str, StoreStatus> {
        self.status.lock().clone()
    }
}

/// Resolves the environment, files and the stores' cached values.
#[derive(Debug)]
pub struct Resolver {
    stores: Arc<ExternalSecrets>,
}

impl Resolver {
    #[must_use]
    pub fn new(stores: Arc<ExternalSecrets>) -> Resolver {
        Resolver { stores }
    }
}

impl SecretSource for Resolver {
    fn env(&self, name: &str) -> Option<String> {
        SystemSecrets.env(name)
    }

    fn file(&self, path: &str) -> std::io::Result<String> {
        SystemSecrets.file(path)
    }

    fn store(&self, store: &str, reference: &str) -> Result<String, String> {
        self.stores.lookup(store, reference)
    }
}

/// What a store value no read has fetched resolves to under [`Unread`].
const UNREAD: &str = "unread-store-value";

/// Resolves as `0` does, but takes a store value it has not read as read: what a check that must
/// not contact the stores resolves with (a validation, a diff, a dry run), so their references are
/// checked by their syntax and the stores they name only.
#[derive(Debug)]
pub struct Unread(pub Arc<dyn SecretSource>);

impl SecretSource for Unread {
    fn env(&self, name: &str) -> Option<String> {
        self.0.env(name)
    }

    fn file(&self, path: &str) -> std::io::Result<String> {
        self.0.file(path)
    }

    fn store(&self, store: &str, reference: &str) -> Result<String, String> {
        Ok(self
            .0
            .store(store, reference)
            .unwrap_or_else(|_| UNREAD.to_owned()))
    }
}

/// Reads what `doc` references on a node that has no runtime yet, with a client of its own under
/// `doc`'s egress settings.
pub async fn prefetch_at_startup(stores: &ExternalSecrets, doc: &Value) {
    if store_references(doc).is_empty() {
        return;
    }
    let client = EgressClient::new(Duration::from_secs(10));
    let security = crate::security::SecurityRt::compile(doc).unwrap_or_default();
    let Some(tls) = security.egress_tls.clone() else {
        return;
    };
    let tasks = tokio_util::task::TaskTracker::new();
    let clock: Arc<dyn antirion_core::Clock> = Arc::new(antirion_core::SystemClock);
    let cx = Cx {
        creds: Arc::new(antirion_providers::Credentials::new(
            client.clone(),
            Arc::clone(&clock),
            tasks.clone(),
        )),
        client,
        policy: Arc::clone(&security.egress),
        tls,
    };
    stores.prefetch(&cx, doc, clock.now()).await;
    tasks.close();
    tasks.wait().await;
}

impl Runtime {
    /// What this node reads the stores with under the serving egress settings.
    pub(crate) fn store_cx(&self) -> Option<Cx> {
        let snap = self.gens.load();
        Some(Cx {
            client: self.egress.clone(),
            creds: Arc::clone(&self.creds),
            policy: Arc::clone(&snap.security.egress),
            tls: snap.security.egress_tls.clone()?,
        })
    }

    /// Reads the store values `doc` needs and the cache lacks; run before validating or building it.
    pub async fn prefetch_secrets(&self, doc: &Value) {
        if let Some(cx) = self.store_cx() {
            self.secret_stores.prefetch(&cx, doc, self.now()).await;
        }
    }
}

/// Reads values that are due, until shutdown; a changed value rebuilds the serving snapshot.
pub async fn run(rt: Arc<Runtime>) {
    loop {
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = tokio::time::sleep(POLL) => {}
        }
        let snap = rt.gens.load();
        if store_references(&snap.config.doc).is_empty() && rt.secret_stores.status().is_empty() {
            continue;
        }
        let Some(cx) = rt.store_cx() else { continue };
        let refreshed = rt
            .secret_stores
            .read_due(&cx, &snap.config.doc, rt.now(), false)
            .await;
        for (store, why) in refreshed.raise {
            let scope = BTreeMap::from([("store".to_owned(), store.to_owned())]);
            let summary =
                format!("The {store} secret store could not be read ({why}); the last values stay in use.");
            tracing::warn!(store, reason = %why, "a secret store could not be read");
            crate::alerting::raise_builtin(&rt, ALERT_RULE, "availability", "warning", scope, summary).await;
        }
        for store in refreshed.recovered {
            let scope = BTreeMap::from([("store".to_owned(), store.to_owned())]);
            crate::alerting::resolve_builtin(&rt, ALERT_RULE, &scope, "system").await;
        }
        if refreshed.changed {
            crate::secretwatch::rebuild(&rt, snap.generation, "values re-read from the secret stores").await;
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn scalars_are_secret_text() {
        assert_eq!(text_of(&json!("s")).as_deref(), Some("s"));
        assert_eq!(text_of(&json!(42)).as_deref(), Some("42"));
        assert_eq!(text_of(&json!(true)).as_deref(), Some("true"));
        assert_eq!(text_of(&json!({"a": 1})), None);
    }

    #[test]
    fn lookups_read_only_the_cache() {
        let stores = ExternalSecrets::new();
        assert!(
            stores
                .lookup("vault", "p#f")
                .unwrap_err()
                .contains("not been read")
        );
        stores.entries.lock().insert(
            key("vault", "p#f"),
            Entry {
                store: "vault",
                fingerprint: 1,
                value: Some(Arc::new(Secret::new("v1"))),
                fetched: Some(Instant::now()),
                ttl: Duration::from_secs(60),
                error: Some("kept after a failed refresh".to_owned()),
            },
        );
        assert_eq!(stores.lookup("vault", "p#f").unwrap(), "v1");
        let resolver = Resolver::new(Arc::new(stores));
        assert!(antirion_config::secrets::expand("${vault:q#f}", &resolver).is_err());
    }
}

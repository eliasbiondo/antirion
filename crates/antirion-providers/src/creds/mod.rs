//! Upstream credentials that must be acquired: GCP and Azure AD access tokens, AWS credentials from
//! the default chain and from STS `AssumeRole`. One cache entry per credential configuration, shared
//! by every deployment using it; at most one acquisition per entry is in flight.

mod assume;
mod aws;
mod azure;
mod gcp;
pub(crate) mod http;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use antirion_core::Clock;
use antirion_egress::{EgressClient, EgressPolicy, TlsProfile};

use self::http::CredClient;
use parking_lot::Mutex;
use tokio_util::task::TaskTracker;

pub use self::assume::AssumeRole;
pub use self::azure::AzureSource;
pub(crate) use self::azure::{DEFAULT_AUTHORITY as AZURE_AUTHORITY, DEFAULT_SCOPE as AZURE_SCOPE};
pub(crate) use self::gcp::DEFAULT_SCOPE;
pub use self::gcp::{GcpSource, ServiceAccount, ServiceAccountError};

/// A source of bearer access tokens.
#[derive(Clone, Debug)]
pub enum TokenSource {
    Gcp(Arc<GcpSource>),
    Azure(Arc<AzureSource>),
}

impl TokenSource {
    fn cache_key(&self) -> String {
        match self {
            TokenSource::Gcp(s) => s.cache_key(),
            TokenSource::Azure(s) => s.cache_key(),
        }
    }

    async fn fetch(&self, client: &CredClient, tls: &TlsProfile, issued: i64) -> Result<Token, CredError> {
        match self {
            TokenSource::Gcp(s) => gcp::fetch(client, tls, s, issued).await,
            TokenSource::Azure(s) => azure::fetch(client, tls, s, issued).await,
        }
    }
}
use crate::sigv4::AwsCredentials;

/// Why no credential could be obtained.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CredError {
    #[error("the credential endpoint is not a valid URL or is not allowed")]
    Endpoint,
    #[error("the credential endpoint could not be reached")]
    Unreachable,
    #[error("the credential endpoint answered with status {0}")]
    Rejected(u16),
    #[error("the credential endpoint's answer has no usable credential")]
    Malformed,
    #[error("no credential source is available")]
    Unavailable,
    #[error("this credential source is not supported")]
    Unsupported,
    #[error("the token assertion could not be signed")]
    Signing,
    #[error("the credential was rejected by the upstream and is not used until it is checked again")]
    Dead,
}

/// An access token; times are Unix seconds on the gateway clock.
#[derive(Clone, Debug)]
pub(crate) struct Token {
    pub(crate) value: String,
    pub(crate) issued: i64,
    pub(crate) expires: i64,
}

impl Token {
    /// `expires − min(300 s, lifetime / 2)`.
    fn refresh_at(&self) -> i64 {
        let lifetime = (self.expires - self.issued).max(0);
        self.expires - (lifetime / 2).min(300)
    }
}

/// How long a credential the upstream rejected twice stays unused.
const DEAD_FOR: i64 = 60;
const MAX_BACKOFF: i64 = 30;

#[derive(Debug, Default)]
struct Retry {
    at: i64,
    backoff: i64,
}

#[derive(Debug, Default)]
struct Slot {
    current: Mutex<Option<Token>>,
    flight: tokio::sync::Mutex<()>,
    background: AtomicBool,
    retry: Mutex<Retry>,
    dead_until: Mutex<Option<i64>>,
}

impl Slot {
    fn valid(&self, now: i64) -> Option<Token> {
        self.current.lock().clone().filter(|t| now < t.expires)
    }

    fn failed(&self, now: i64) {
        let mut r = self.retry.lock();
        r.backoff = if r.backoff == 0 {
            1
        } else {
            (r.backoff * 2).min(MAX_BACKOFF)
        };
        r.at = now + r.backoff;
    }

    fn store(&self, token: Token) {
        *self.current.lock() = Some(token);
        *self.retry.lock() = Retry::default();
    }
}

/// AWS credentials resolved from the default chain, per region.
#[derive(Debug)]
struct AwsEntry {
    creds: AwsCredentials,
    expires: Option<i64>,
    fetched: i64,
}

/// How long credentials without an expiry (environment, shared file) are reused before the chain is
/// read again.
const AWS_REREAD: i64 = 300;

/// The credential cache and the credential-acquisition client.
pub struct Credentials {
    client: EgressClient,
    clock: Arc<dyn Clock>,
    tasks: TaskTracker,
    slots: Mutex<HashMap<String, Arc<Slot>>>,
    aws: Mutex<HashMap<String, Arc<AwsEntry>>>,
    aws_flight: tokio::sync::Mutex<()>,
    /// Role credentials from STS, by role, base credentials and region.
    assumed: Mutex<HashMap<String, Arc<AwsEntry>>>,
    assume_flight: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").finish_non_exhaustive()
    }
}

/// Unix seconds on the real clock, for protocol timestamps (assertions, credential expiry).
pub(crate) fn unix_now() -> i64 {
    antirion_core::SystemClock.now().0 / 1_000_000
}

/// The state of one token cache entry, for the runtime view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenState {
    pub state: &'static str,
    pub expires_at: Option<i64>,
}

impl Credentials {
    #[must_use]
    pub fn new(client: EgressClient, clock: Arc<dyn Clock>, tasks: TaskTracker) -> Credentials {
        Credentials {
            client,
            clock,
            tasks,
            slots: Mutex::new(HashMap::new()),
            aws: Mutex::new(HashMap::new()),
            aws_flight: tokio::sync::Mutex::new(()),
            assumed: Mutex::new(HashMap::new()),
            assume_flight: tokio::sync::Mutex::new(()),
        }
    }

    fn now(&self) -> i64 {
        self.clock.now().0 / 1_000_000
    }

    fn via(&self, policy: &Arc<EgressPolicy>) -> CredClient {
        CredClient {
            client: self.client.clone(),
            policy: Arc::clone(policy),
        }
    }

    fn slot(&self, key: &str) -> Arc<Slot> {
        Arc::clone(self.slots.lock().entry(key.to_owned()).or_default())
    }

    /// An access token for `source`: the cached one while it is valid (refreshed in the background
    /// after its refresh time), else a new one, fetched once for every waiter. Configurable token
    /// endpoints are reached under `policy`, the serving egress policy.
    pub async fn token(
        &self,
        source: &TokenSource,
        tls: &TlsProfile,
        policy: &Arc<EgressPolicy>,
    ) -> Result<String, CredError> {
        let slot = self.slot(&source.cache_key());
        let now = self.now();
        if slot.dead_until.lock().is_some_and(|d| now < d) {
            return Err(CredError::Dead);
        }
        if let Some(t) = slot.valid(now) {
            if now >= t.refresh_at() && now >= slot.retry.lock().at {
                self.refresh_in_background(&slot, source, tls, policy);
            }
            return Ok(t.value);
        }
        let _flight = slot.flight.lock().await;
        let now = self.now();
        if let Some(t) = slot.valid(now) {
            return Ok(t.value);
        }
        match source.fetch(&self.via(policy), tls, now).await {
            Ok(t) => {
                let value = t.value.clone();
                slot.store(t);
                Ok(value)
            }
            Err(e) => {
                slot.failed(now);
                Err(e)
            }
        }
    }

    fn refresh_in_background(
        &self,
        slot: &Arc<Slot>,
        source: &TokenSource,
        tls: &TlsProfile,
        policy: &Arc<EgressPolicy>,
    ) {
        if slot.background.swap(true, Ordering::AcqRel) {
            return;
        }
        let (slot, source, tls) = (Arc::clone(slot), source.clone(), tls.clone());
        let (client, clock) = (self.via(policy), Arc::clone(&self.clock));
        self.tasks.spawn(async move {
            let _flight = slot.flight.lock().await;
            let now = clock.now().0 / 1_000_000;
            match source.fetch(&client, &tls, now).await {
                Ok(t) => slot.store(t),
                Err(e) => {
                    tracing::warn!(error = %e, "token refresh failed; the current token stays in use");
                    slot.failed(now);
                }
            }
            slot.background.store(false, Ordering::Release);
        });
    }

    /// Drops `token` from the cache after the upstream rejected it, so the next request fetches a
    /// new one.
    pub fn invalidate(&self, source: &TokenSource, token: &str) {
        let slot = self.slot(&source.cache_key());
        let mut cur = slot.current.lock();
        if cur.as_ref().is_some_and(|t| t.value == token) {
            *cur = None;
        }
    }

    /// Stops using a credential the upstream rejected again after a refresh.
    pub fn mark_dead(&self, source: &TokenSource) {
        let slot = self.slot(&source.cache_key());
        *slot.current.lock() = None;
        *slot.dead_until.lock() = Some(self.now() + DEAD_FOR);
    }

    /// The cache state of a token source.
    #[must_use]
    pub fn token_state(&self, source: &TokenSource) -> TokenState {
        let Some(slot) = self.slots.lock().get(&source.cache_key()).cloned() else {
            return TokenState {
                state: "empty",
                expires_at: None,
            };
        };
        let now = self.now();
        if slot.dead_until.lock().is_some_and(|d| now < d) {
            return TokenState {
                state: "failed",
                expires_at: None,
            };
        }
        match slot.current.lock().as_ref() {
            Some(t) if now < t.refresh_at() => TokenState {
                state: "valid",
                expires_at: Some(t.expires),
            },
            Some(t) if now < t.expires => TokenState {
                state: "refreshing",
                expires_at: Some(t.expires),
            },
            _ => TokenState {
                state: "expired",
                expires_at: None,
            },
        }
    }

    /// AWS credentials: the configured static ones, else the default chain for `region` (cached
    /// until shortly before they expire).
    pub async fn aws(
        &self,
        configured: Option<&AwsCredentials>,
        region: &str,
        tls: &TlsProfile,
        policy: &Arc<EgressPolicy>,
    ) -> Result<AwsCredentials, CredError> {
        if let Some(c) = configured {
            return Ok(c.clone());
        }
        let now = unix_now();
        let fresh = |e: &AwsEntry| match e.expires {
            Some(exp) => now < exp - 300,
            None => now < e.fetched + AWS_REREAD,
        };
        if let Some(e) = self.aws.lock().get(region).filter(|e| fresh(e)) {
            return Ok(e.creds.clone());
        }
        let _flight = self.aws_flight.lock().await;
        if let Some(e) = self.aws.lock().get(region).filter(|e| fresh(e)) {
            return Ok(e.creds.clone());
        }
        let f = aws::chain(&self.via(policy), tls, region).await?;
        let entry = Arc::new(AwsEntry {
            creds: f.creds.clone(),
            expires: f.expires,
            fetched: now,
        });
        self.aws.lock().insert(region.to_owned(), entry);
        Ok(f.creds)
    }
}

impl Credentials {
    /// Credentials for `role`, assumed with the base credentials (static or from the default
    /// chain) and reused until shortly before they expire.
    pub async fn aws_assumed(
        &self,
        configured: Option<&AwsCredentials>,
        role: &AssumeRole,
        region: &str,
        tls: &TlsProfile,
        policy: &Arc<EgressPolicy>,
    ) -> Result<AwsCredentials, CredError> {
        let base = self.aws(configured, region, tls, policy).await?;
        let key = role.cache_key(&base.access_key_id, region);
        let fresh = |e: &AwsEntry| {
            let now = unix_now();
            match e.expires {
                Some(exp) => now < exp - (exp - e.fetched).clamp(0, 600) / 2,
                None => now < e.fetched + AWS_REREAD,
            }
        };
        if let Some(e) = self.assumed.lock().get(&key).filter(|e| fresh(e)) {
            return Ok(e.creds.clone());
        }
        let _flight = self.assume_flight.lock().await;
        if let Some(e) = self.assumed.lock().get(&key).filter(|e| fresh(e)) {
            return Ok(e.creds.clone());
        }
        let now = unix_now();
        let f = assume::assume(&self.via(policy), tls, role, &base, region, now).await?;
        let entry = Arc::new(AwsEntry {
            creds: f.creds.clone(),
            expires: f.expires,
            fetched: now,
        });
        self.assumed.lock().insert(key, entry);
        Ok(f.creds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_time() {
        let t = Token {
            value: String::new(),
            issued: 0,
            expires: 3600,
        };
        assert_eq!(t.refresh_at(), 3300);
        let short = Token {
            value: String::new(),
            issued: 0,
            expires: 100,
        };
        assert_eq!(short.refresh_at(), 50);
    }

    #[test]
    fn backoff_doubles_to_thirty_seconds() {
        let s = Slot::default();
        let mut seen = Vec::new();
        for _ in 0..7 {
            s.failed(0);
            seen.push(s.retry.lock().at);
        }
        assert_eq!(seen, [1, 2, 4, 8, 16, 30, 30]);
    }
}

//! Browser push notifications: Web Push (RFC 8030) to the push service of each browser a user turned
//! notifications on in, with message encryption (RFC 8291) and the gateway's VAPID identity
//! (RFC 8292). A subscription is an identity record of its user, its endpoint and keys sealed at
//! rest; messages go through one queue whose worker sends each as client-influenced egress (the
//! endpoint is the browser's to choose) and retries what the push service may accept later.

mod deliver;
mod ece;
mod vapid;

use std::sync::Arc;

use antirion_core::UtcMicros;
use antirion_egress::TrustClass;
use antirion_egress::ssrf::validate_url;
use antirion_identity::verifier::random_id;
use antirion_store::{IdentityRecord, StoreError};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use self::deliver::{Pusher, TestOutcome, run};
use self::ece::Receiver;
use self::vapid::Vapid;
use crate::Runtime;

/// The identity record kind of a subscription.
const SUBSCRIPTION: &str = "push_subscription";
/// Where the cluster's VAPID key pair is kept, sealed.
const VAPID_KEY: &str = "push/vapid";
/// The most browsers one user receives notifications in.
const MAX_PER_USER: usize = 10;

/// Why a push call was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error("{0}")]
    Endpoint(String),
    #[error("{0}")]
    Keys(&'static str),
    #[error("a user receives notifications in at most {MAX_PER_USER} browsers")]
    TooMany,
    #[error("the store cannot be read or written: {0}")]
    Store(#[from] StoreError),
    #[error("the gateway's push key is not available")]
    Key,
}

/// What a reader sees of one subscription: never its endpoint or keys.
#[derive(Clone, Debug, Serialize)]
pub struct PushSubscription {
    pub id: String,
    /// The push service's origin (`https://fcm.googleapis.com`).
    pub origin: String,
    /// What the browser called itself when it subscribed.
    pub device: Option<String>,
    pub created_at: String,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
}

/// The stored record: the shown facts and the sealed endpoint and keys.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Stored {
    origin: String,
    device: Option<String>,
    created_at: String,
    last_success_at: Option<String>,
    last_error: Option<String>,
    sealed: String,
}

/// The sealed part of a subscription.
#[derive(Serialize, Deserialize)]
struct Secret {
    endpoint: String,
    p256dh: String,
    auth: String,
}

/// One notification: what the console's service worker shows.
#[derive(Clone, Debug, Serialize)]
pub struct PushMessage {
    pub title: String,
    pub body: String,
    /// The console page a click opens.
    pub url: Option<String>,
    /// Replaces an earlier notification with the same tag (an alert instance's id).
    pub tag: Option<String>,
    /// `high`, `normal` or `low`: the RFC 8030 urgency.
    #[serde(skip)]
    pub urgency: &'static str,
}

fn seal_location(id: &str) -> String {
    format!("{SUBSCRIPTION}/{id}")
}

fn origin_of(endpoint: &str) -> Result<String, PushError> {
    // The address rules of the serving policy are applied to each message as it is sent; here the
    // endpoint only has to be a well-formed https URL.
    let v = validate_url(
        endpoint,
        TrustClass::Operator,
        &antirion_egress::EgressPolicy::default(),
    )
    .ok()
    .filter(|v| v.scheme == "https")
    .ok_or_else(|| PushError::Endpoint("endpoint must be an https URL of a push service".to_owned()))?;
    let host = if v.host.contains(':') {
        format!("[{}]", v.host)
    } else {
        v.host
    };
    Ok(if v.port == 443 {
        format!("https://{host}")
    } else {
        format!("https://{host}:{}", v.port)
    })
}

fn decode_key(value: &str, field: &'static str) -> Result<Vec<u8>, PushError> {
    URL_SAFE_NO_PAD
        .decode(value.trim_end_matches('='))
        .map_err(|_| PushError::Keys(field))
}

fn shown(id: &str, s: &Stored) -> PushSubscription {
    PushSubscription {
        id: id.to_owned(),
        origin: s.origin.clone(),
        device: s.device.clone(),
        created_at: s.created_at.clone(),
        last_success_at: s.last_success_at.clone(),
        last_error: s.last_error.clone(),
    }
}

impl Runtime {
    async fn vapid(&self) -> Result<Arc<Vapid>, PushError> {
        let pusher = &self.state.console.push;
        if let Some(v) = pusher.vapid.get() {
            return Ok(Arc::clone(v));
        }
        let fresh = Vapid::generate().map_err(|_| PushError::Key)?;
        let sealed = self.sealer.seal(VAPID_KEY, &fresh).map_err(|_| PushError::Key)?;
        let stored = self.store.get_or_init(VAPID_KEY, sealed.into_bytes()).await?;
        let pkcs8 = self
            .sealer
            .open_value(VAPID_KEY, stored)
            .map_err(|_| PushError::Key)?;
        let v = Arc::new(Vapid::from_pkcs8(&pkcs8).map_err(|_| PushError::Key)?);
        Ok(Arc::clone(pusher.vapid.get_or_init(|| v)))
    }

    /// The gateway's VAPID public key, which a browser subscribes with (`applicationServerKey`).
    pub async fn push_public_key(&self) -> Result<String, PushError> {
        Ok(self.vapid().await?.public_key())
    }

    async fn stored_subscriptions(&self, user: &str) -> Result<Vec<(IdentityRecord, Stored)>, PushError> {
        Ok(self
            .store
            .records(SUBSCRIPTION, Some(user))
            .await?
            .into_iter()
            .filter_map(|r| {
                let s = serde_json::from_str::<Stored>(&r.body).ok()?;
                Some((r, s))
            })
            .collect())
    }

    /// The browsers `user` receives notifications in, oldest first.
    pub async fn push_subscriptions(&self, user: &str) -> Result<Vec<PushSubscription>, PushError> {
        let mut out: Vec<PushSubscription> = self
            .stored_subscriptions(user)
            .await?
            .iter()
            .map(|(r, s)| shown(&r.id, s))
            .collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    /// Adds a browser's subscription for `user`; the same endpoint subscribed again replaces it.
    pub async fn subscribe_push(
        &self,
        user: &str,
        endpoint: &str,
        p256dh: &str,
        auth: &str,
        device: Option<&str>,
    ) -> Result<PushSubscription, PushError> {
        let origin = origin_of(endpoint)?;
        let (public, secret) = (decode_key(p256dh, "keys.p256dh")?, decode_key(auth, "keys.auth")?);
        ece::check_keys(&Receiver {
            public: &public,
            auth: &secret,
        })
        .map_err(|e| match e {
            ece::EceError::AuthSecret => PushError::Keys("keys.auth must be 16 bytes, base64url"),
            _ => PushError::Keys("keys.p256dh must be an uncompressed P-256 public key, base64url"),
        })?;
        let existing = self.stored_subscriptions(user).await?;
        let mut replaced = None;
        for (r, s) in &existing {
            let same = self
                .sealer
                .open(&seal_location(&r.id), &s.sealed)
                .ok()
                .and_then(|p| serde_json::from_slice::<Secret>(&p).ok())
                .is_some_and(|sec| sec.endpoint == endpoint);
            if same {
                replaced = Some(r.id.clone());
            }
        }
        if replaced.is_none() && existing.len() >= MAX_PER_USER {
            return Err(PushError::TooMany);
        }
        let id = match replaced {
            Some(id) => id,
            None => format!("ps_{}", random_id().map_err(|_| PushError::Key)?),
        };
        let secret = Secret {
            endpoint: endpoint.to_owned(),
            p256dh: URL_SAFE_NO_PAD.encode(&public),
            auth: URL_SAFE_NO_PAD.encode(&secret),
        };
        let plain = serde_json::to_vec(&secret).map_err(|_| PushError::Key)?;
        let stored = Stored {
            origin,
            device: device.map(|d| d.chars().take(100).collect()),
            created_at: self.now().to_rfc3339_seconds(),
            last_success_at: None,
            last_error: None,
            sealed: self
                .sealer
                .seal(&seal_location(&id), &plain)
                .map_err(|_| PushError::Key)?,
        };
        self.store
            .put_record(IdentityRecord {
                kind: SUBSCRIPTION.to_owned(),
                id: id.clone(),
                user: user.to_owned(),
                expires_us: None,
                body: serde_json::to_string(&stored).map_err(|_| PushError::Key)?,
            })
            .await?;
        Ok(shown(&id, &stored))
    }

    /// Removes one of `user`'s subscriptions; false when they have none of that id.
    pub async fn unsubscribe_push(&self, user: &str, id: &str) -> Result<bool, PushError> {
        match self.store.record(SUBSCRIPTION, id).await? {
            Some(r) if r.user == user => Ok(self.store.delete_record(SUBSCRIPTION, id).await?),
            _ => Ok(false),
        }
    }

    /// Encrypts `message` for each of `user`'s subscriptions: `(id, endpoint, body)`.
    async fn sealed_messages(
        &self,
        user: &str,
        message: &PushMessage,
    ) -> Result<Vec<deliver::Job>, PushError> {
        let mut payload = serde_json::to_vec(message).map_err(|_| PushError::Key)?;
        if payload.len() > ece::MAX_PAYLOAD {
            let mut short = message.clone();
            short.body = short.body.chars().take(500).collect();
            payload = serde_json::to_vec(&short).map_err(|_| PushError::Key)?;
        }
        let mut jobs = Vec::new();
        for (r, s) in self.stored_subscriptions(user).await? {
            let Some(secret) = self
                .sealer
                .open(&seal_location(&r.id), &s.sealed)
                .ok()
                .and_then(|p| serde_json::from_slice::<Secret>(&p).ok())
            else {
                continue;
            };
            let (Ok(public), Ok(auth)) = (
                decode_key(&secret.p256dh, "p256dh"),
                decode_key(&secret.auth, "auth"),
            ) else {
                continue;
            };
            let Ok(body) = ece::encrypt(
                &Receiver {
                    public: &public,
                    auth: &auth,
                },
                &payload,
            ) else {
                continue;
            };
            jobs.push(deliver::Job {
                id: r.id,
                origin: s.origin,
                endpoint: secret.endpoint,
                body,
                urgency: message.urgency,
            });
        }
        Ok(jobs)
    }

    /// Queues `message` to every browser `user` subscribed; nothing waits for a push service.
    pub async fn push_to(&self, user: &str, message: &PushMessage) {
        match self.sealed_messages(user, message).await {
            Ok(jobs) => {
                for job in jobs {
                    self.state.console.push.enqueue(job, &self.metrics);
                }
            }
            Err(e) => tracing::warn!(error = %e, "a push notification could not be prepared"),
        }
    }

    /// Sends a test notification to each of `user`'s browsers now and says how each went.
    pub async fn push_test(&self, user: &str) -> Result<Vec<TestOutcome>, PushError> {
        let message = PushMessage {
            title: "Antirion".to_owned(),
            body: "Notifications from this gateway reach this browser.".to_owned(),
            url: None,
            tag: Some("antirion-test".to_owned()),
            urgency: "normal",
        };
        let jobs = self.sealed_messages(user, &message).await?;
        let mut out = Vec::new();
        for job in jobs {
            out.push(deliver::once(self, &job).await);
        }
        Ok(out)
    }

    /// Records how the last message to a subscription went.
    async fn note_delivery(&self, id: &str, error: Option<String>) {
        let Ok(Some(r)) = self.store.record(SUBSCRIPTION, id).await else {
            return;
        };
        let Ok(mut s) = serde_json::from_str::<Stored>(&r.body) else {
            return;
        };
        match error {
            None => {
                s.last_success_at = Some(self.now().to_rfc3339_seconds());
                s.last_error = None;
            }
            Some(e) => s.last_error = Some(e),
        }
        if let Ok(body) = serde_json::to_string(&s)
            && let Err(e) = self.store.swap_record(SUBSCRIPTION, id, &r.body, &body).await
        {
            tracing::debug!(error = %e, "a push subscription's last delivery could not be recorded");
        }
    }

    /// The VAPID `sub`: the console's https origin, else the outgoing mail address.
    fn push_subject(&self) -> Option<String> {
        let cfg = self.alerting();
        if let Some(origin) = cfg.console_origin.as_ref().filter(|o| o.starts_with("https://")) {
            return Some(origin.clone());
        }
        let snap = self.gens.load();
        let mail = snap.identity.console.mail.as_ref()?;
        let from = mail
            .from
            .rsplit_once('<')
            .map_or(mail.from.as_str(), |(_, a)| a.trim_end_matches('>'));
        Some(format!("mailto:{}", from.trim()))
    }

    /// The VAPID token's clock: the push service checks `exp` against real time, so the test clock
    /// never applies.
    fn now_secs(&self) -> i64 {
        let UtcMicros(us) = antirion_core::Clock::now(&antirion_core::SystemClock);
        us / 1_000_000
    }
}

/// The urgency of an alert of `severity`.
#[must_use]
pub fn urgency_of(severity: &str) -> &'static str {
    match severity {
        "critical" => "high",
        "warning" => "normal",
        _ => "low",
    }
}

/// Whether a user's preferences ask for browser push of an alert of `severity` and `category`: the
/// same shape as their email choice (`notifications.push.{severities, categories}`).
#[must_use]
pub fn wanted(prefs: &Value, severity: &str, category: &str) -> bool {
    let Some(push) = prefs.pointer("/notifications/push") else {
        return false;
    };
    let listed = |member: &str, value: &str| {
        push.get(member)
            .and_then(Value::as_array)
            .is_some_and(|l| l.iter().any(|v| v.as_str() == Some(value)))
    };
    let categories_ok = push
        .get("categories")
        .and_then(Value::as_array)
        .is_none_or(|l| l.is_empty() || listed("categories", category));
    listed("severities", severity) && categories_ok
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn endpoints_are_https_and_shown_by_origin() {
        assert_eq!(
            origin_of("https://fcm.googleapis.com/fcm/send/abc")
                .ok()
                .as_deref(),
            Some("https://fcm.googleapis.com")
        );
        assert_eq!(
            origin_of("https://127.0.0.1:9101/push/x").ok().as_deref(),
            Some("https://127.0.0.1:9101")
        );
        for bad in [
            "http://push.example/x",
            "ftp://push.example/x",
            "https://u:p@push.example/x",
            "push",
        ] {
            assert!(origin_of(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn preferences_choose_push_like_email() {
        let prefs =
            json!({"notifications": {"push": {"severities": ["critical"], "categories": ["budget"]}}});
        assert!(wanted(&prefs, "critical", "budget"));
        assert!(!wanted(&prefs, "critical", "latency"));
        assert!(!wanted(&prefs, "warning", "budget"));
        assert!(!wanted(
            &json!({"notifications": {"email": {"severities": ["critical"]}}}),
            "critical",
            "x"
        ));
        assert_eq!(urgency_of("critical"), "high");
    }
}

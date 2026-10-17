//! Admin-plane authentication: the bootstrap credential, admin tokens (with their user's bindings and
//! their own), and sessions started with either. Virtual keys never authenticate here.

use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_core::audit::ActorKind;
use antirion_identity::rbac::{AdminPrincipal, Binding, Role, Scope};
use antirion_identity::verifier::{digest_hex, random_id};
use antirion_identity::{Credential, Plane};
use antirion_store::{AdminTokenRecord, SessionRecord, StoreError};
use serde::{Deserialize, Serialize};

use crate::Runtime;

/// The actor id of the bootstrap credential.
pub const BOOTSTRAP: &str = "bootstrap";

/// The prefix of a session's credential id when the session belongs to a user who signed in
/// through single sign-on.
const USER_SESSION: &str = "user:";

/// A token's last use is recorded at most once a minute.
const TOUCH_EVERY_US: i64 = 60_000_000;

/// Where a sign-in came from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Client {
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    /// The organization named at sign-in: the console's starting scope, granting nothing.
    pub organization: Option<String>,
}

/// How a session was started, kept with it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFacts {
    /// `token`, `oidc`, `oauth`, `saml` or `local`.
    pub method: String,
    #[serde(default)]
    pub mfa: bool,
    #[serde(default)]
    pub trusted: bool,
    #[serde(default)]
    pub organization: Option<String>,
    #[serde(default)]
    pub client_ip: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    /// The identity provider of a single sign-on, by its name in the configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

impl SessionFacts {
    #[must_use]
    pub fn new(method: &str, mfa: bool, trusted: bool, client: Client) -> SessionFacts {
        SessionFacts {
            method: method.to_owned(),
            mfa,
            trusted,
            organization: client.organization,
            client_ip: client.ip,
            user_agent: client.user_agent,
            provider: None,
        }
    }

    /// The same facts, for a sign-in through the named identity provider.
    #[must_use]
    pub fn with_provider(self, provider: &str) -> SessionFacts {
        SessionFacts {
            provider: Some(provider.to_owned()),
            ..self
        }
    }

    fn parse(text: &str) -> SessionFacts {
        serde_json::from_str::<SessionFacts>(text)
            .ok()
            .filter(|f| !f.method.is_empty())
            .unwrap_or_else(|| SessionFacts {
                method: "token".to_owned(),
                ..SessionFacts::default()
            })
    }
}

/// A live session and its deadlines.
#[derive(Clone, Debug)]
pub struct Session {
    pub principal: AdminPrincipal,
    pub idle_expires_at: UtcMicros,
    pub expires_at: UtcMicros,
    /// The digest of its id, under which the store keeps it.
    pub digest: String,
    pub facts: SessionFacts,
}

impl Session {
    fn of(
        r: &SessionRecord,
        principal: AdminPrincipal,
        facts: SessionFacts,
        now: UtcMicros,
        idle: Duration,
        absolute: Duration,
    ) -> Session {
        let absolute_end = r.created_us.saturating_add(micros(absolute));
        Session {
            principal,
            idle_expires_at: UtcMicros(now.0.saturating_add(micros(idle)).min(absolute_end)),
            expires_at: UtcMicros(absolute_end),
            digest: r.id_digest.clone(),
            facts,
        }
    }
}

/// A session as its lists show it.
#[derive(Clone, Debug)]
pub struct SessionInfo {
    /// A public identifier, derived from the digest: never the cookie value.
    pub id: String,
    pub digest: String,
    pub created_at: UtcMicros,
    pub last_seen_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub facts: SessionFacts,
}

/// The identifier lists show for the session stored under `digest`.
#[must_use]
pub fn public_id(digest: &str) -> String {
    let d = digest_hex(&format!("session:{digest}"));
    d.get(..24).unwrap_or(&d).to_owned()
}

fn live(r: &SessionRecord, now: UtcMicros, idle: Duration, absolute: Duration) -> bool {
    now.0 < r.last_used_us.saturating_add(micros(idle))
        && now.0 < r.created_us.saturating_add(micros(absolute))
}

/// Why no session was started.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("the credential is not valid")]
    InvalidCredential,
    #[error("no session id could be generated")]
    Random,
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn micros(d: Duration) -> i64 {
    i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
}

impl Runtime {
    fn bootstrap_principal() -> AdminPrincipal {
        AdminPrincipal {
            kind: ActorKind::Token,
            id: BOOTSTRAP.to_owned(),
            user: None,
            bindings: vec![Binding {
                role: Role::GatewayAdmin,
                scope: Scope::Gateway,
            }],
            read_only: false,
        }
    }

    /// The principal of a stored token: unexpired, its user enabled, with the user's bindings and
    /// the token's own.
    async fn token_principal(&self, record: AdminTokenRecord) -> Option<AdminPrincipal> {
        if let Some(exp) = record
            .expires_at
            .as_deref()
            .and_then(|e| UtcMicros::parse_rfc3339(e).ok())
            && self.now() >= exp
        {
            return None;
        }
        let snap = self.gens.load();
        let user = snap.identity.users.get(&record.user).filter(|u| u.enabled)?;
        let mut bindings = user.bindings.clone();
        let own = self.store.token_bindings(Some(record.kid.clone())).await.ok()?;
        bindings.extend(own.iter().filter_map(|b| {
            Some(Binding {
                role: snap.identity.roles.resolve(&b.role)?,
                scope: Scope::parse(&b.scope)?,
            })
        }));
        Some(AdminPrincipal {
            kind: ActorKind::Token,
            read_only: record.access.as_deref() == Some("read_only"),
            id: record.kid,
            user: Some(record.user),
            bindings,
        })
    }

    /// Authenticates a bearer credential: the bootstrap token, or an admin token whose user is
    /// enabled.
    pub async fn authenticate_admin(&self, token: &str) -> Option<AdminPrincipal> {
        if let Some(v) = &self.bootstrap_verifier
            && self.pepper.verify(token, v)
        {
            return Some(Self::bootstrap_principal());
        }
        let cred = Credential::parse(token).filter(|c| c.plane() == Plane::Admin)?;
        let record = self
            .store
            .admin_token(cred.kid().to_owned())
            .await
            .ok()
            .flatten()?;
        if !self.pepper.verify(cred.expose(), &record.verifier) {
            return None;
        }
        let now = self.now();
        if record
            .last_used_us
            .is_none_or(|t| now.0.saturating_sub(t) >= TOUCH_EVERY_US)
            && let Err(e) = self.store.touch_admin_token(record.kid.clone(), now.0).await
        {
            tracing::debug!(error = %e, "a token's last use could not be recorded");
        }
        self.token_principal(record).await
    }

    /// The principal behind a session's credential, checked again on every use so that deleting
    /// the token or disabling its user ends the session too.
    async fn principal_of_kid(&self, kid: &str) -> Option<AdminPrincipal> {
        if let Some(name) = kid.strip_prefix(USER_SESSION) {
            let snap = self.gens.load();
            let user = snap.identity.users.get(name).filter(|u| u.enabled)?;
            return Some(AdminPrincipal {
                kind: ActorKind::Operator,
                id: name.to_owned(),
                user: Some(name.to_owned()),
                bindings: user.bindings.clone(),
                read_only: false,
            });
        }
        if kid == BOOTSTRAP {
            return self
                .bootstrap_verifier
                .as_ref()
                .map(|_| Self::bootstrap_principal());
        }
        let record = self.store.admin_token(kid.to_owned()).await.ok().flatten()?;
        self.token_principal(record).await
    }

    fn limits(&self, facts: &SessionFacts) -> (Duration, Duration) {
        let snap = self.gens.load();
        match snap.identity.console.trusted.filter(|_| facts.trusted) {
            Some(t) => (t.idle, t.max_age),
            None => (
                snap.identity.settings.session_idle,
                snap.identity.settings.session_absolute,
            ),
        }
    }

    /// The cookie lifetime of a new session with these facts.
    #[must_use]
    pub fn session_max_age(&self, facts: &SessionFacts) -> Duration {
        self.limits(facts).1
    }

    async fn open_session(
        &self,
        kid: String,
        principal: AdminPrincipal,
        facts: SessionFacts,
    ) -> Result<(String, Session), SessionError> {
        let id = random_id().map_err(|_| SessionError::Random)?;
        let now = self.now();
        let record = SessionRecord {
            id_digest: digest_hex(&id),
            kid,
            created_us: now.0,
            last_used_us: now.0,
            user: principal.user.clone(),
            facts: serde_json::to_string(&facts).unwrap_or_else(|_| "{}".to_owned()),
        };
        self.store.put_session(record.clone()).await?;
        if let Some(user) = &principal.user {
            self.record_login(user, &facts).await;
        }
        let (idle, absolute) = self.limits(&facts);
        let session = Session::of(&record, principal, facts, now, idle, absolute);
        Ok((id, session))
    }

    /// Starts a session with an admin credential; returns its opaque id once.
    pub async fn start_session(
        &self,
        token: &str,
        client: Client,
    ) -> Result<(String, Session), SessionError> {
        let principal = self
            .authenticate_admin(token)
            .await
            .ok_or(SessionError::InvalidCredential)?;
        let facts = SessionFacts::new("token", false, false, client);
        self.open_session(principal.id.clone(), principal, facts).await
    }

    /// Starts a session for a user who signed in (single sign-on, or a local account after its
    /// second factor).
    pub async fn start_user_session(
        &self,
        user: &str,
        facts: SessionFacts,
    ) -> Result<(String, Session), SessionError> {
        let kid = format!("{USER_SESSION}{user}");
        let principal = self
            .principal_of_kid(&kid)
            .await
            .ok_or(SessionError::InvalidCredential)?;
        self.open_session(kid, principal, facts).await
    }

    /// The live session with this id, which this use keeps from going idle. An expired session, and
    /// a local one whose user may no longer sign in with a password, is deleted.
    pub async fn session(&self, id: &str) -> Result<Option<Session>, StoreError> {
        let digest = digest_hex(id);
        let Some(record) = self.store.session(digest.clone()).await? else {
            return Ok(None);
        };
        let facts = SessionFacts::parse(&record.facts);
        let (idle, absolute) = self.limits(&facts);
        let now = self.now();
        if !live(&record, now, idle, absolute)
            || (facts.method == "local"
                && record
                    .user
                    .as_deref()
                    .is_none_or(|u| self.local_refusal(u).is_some()))
        {
            self.store.delete_session(digest).await?;
            return Ok(None);
        }
        let Some(principal) = self.principal_of_kid(&record.kid).await else {
            return Ok(None);
        };
        self.store.touch_session(digest, now.0).await?;
        let mut touched = record;
        touched.last_used_us = now.0;
        Ok(Some(Session::of(&touched, principal, facts, now, idle, absolute)))
    }

    /// The live sessions acting for `user`, or started with credential `kid`, oldest first.
    pub async fn sessions_of(
        &self,
        user: Option<&str>,
        kid: Option<&str>,
    ) -> Result<Vec<SessionInfo>, StoreError> {
        let now = self.now();
        let records = self
            .store
            .sessions_of(user.map(str::to_owned), kid.map(str::to_owned))
            .await?;
        Ok(records
            .into_iter()
            .filter_map(|r| {
                let facts = SessionFacts::parse(&r.facts);
                let (idle, absolute) = self.limits(&facts);
                live(&r, now, idle, absolute).then(|| SessionInfo {
                    id: public_id(&r.id_digest),
                    expires_at: UtcMicros(r.created_us.saturating_add(micros(absolute))),
                    created_at: UtcMicros(r.created_us),
                    last_seen_at: UtcMicros(r.last_used_us),
                    digest: r.id_digest,
                    facts,
                })
            })
            .collect())
    }

    /// Ends these sessions (by digest); returns how many existed.
    pub async fn end_sessions(&self, digests: Vec<String>) -> Result<u64, StoreError> {
        if digests.is_empty() {
            return Ok(0);
        }
        self.store.delete_sessions(digests).await
    }

    /// Ends every session of a user, except the one with digest `keep`; returns how many ended.
    pub async fn end_user_sessions(&self, user: &str, keep: Option<&str>) -> Result<u64, StoreError> {
        let digests = self
            .store
            .sessions_of(Some(user.to_owned()), Some(format!("{USER_SESSION}{user}")))
            .await?
            .into_iter()
            .map(|r| r.id_digest)
            .filter(|d| Some(d.as_str()) != keep)
            .collect();
        self.end_sessions(digests).await
    }

    /// Ends a session server-side.
    pub async fn end_session(&self, id: &str) -> Result<(), StoreError> {
        self.store.delete_session(digest_hex(id)).await
    }
}

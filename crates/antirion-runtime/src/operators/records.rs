//! Invitations, preferences and email changes: per-user state the Admin API reads and writes as
//! whole records.

use antirion_core::UtcMicros;
use antirion_identity::verifier::{digest_hex, random_id};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{IdentityError, kind, token};
use crate::Runtime;

/// An invitation not yet accepted or cancelled; the user it created stays disabled until it is
/// accepted. Its link may have expired, which a resend renews.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Invitation {
    pub id: String,
    pub user: String,
    pub email: String,
    pub invited_by: String,
    #[serde(with = "micros")]
    pub created_at: UtcMicros,
    #[serde(with = "micros")]
    pub expires_at: UtcMicros,
    #[serde(default)]
    pub role_bindings: Value,
    /// The digest of the token that accepts it; replaced by a resend.
    #[serde(default)]
    token_digest: String,
}

mod micros {
    use antirion_core::UtcMicros;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(t: &UtcMicros, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(t.0)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<UtcMicros, D::Error> {
        i64::deserialize(d).map(UtcMicros)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct TokenOf {
    invitation: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct EmailChange {
    email: String,
}

impl Invitation {
    /// A new invitation for `user`, valid `ttl` from `now`.
    pub fn new(
        user: &str,
        email: &str,
        invited_by: &str,
        role_bindings: Value,
        now: UtcMicros,
        ttl: std::time::Duration,
    ) -> Result<Invitation, IdentityError> {
        let id = random_id().map_err(|_| IdentityError::Internal)?;
        Ok(Invitation {
            id: format!("inv_{}", id.get(..20).unwrap_or(&id)),
            user: user.to_owned(),
            email: email.to_owned(),
            invited_by: invited_by.to_owned(),
            created_at: now,
            expires_at: now.saturating_add(ttl),
            role_bindings,
            token_digest: String::new(),
        })
    }

    /// Whether its link no longer works at `now`.
    #[must_use]
    pub fn expired(&self, now: UtcMicros) -> bool {
        now >= self.expires_at
    }
}

impl Runtime {
    /// Stores an invitation with a fresh token (voiding any earlier one); returns the token.
    pub async fn put_invitation(&self, mut inv: Invitation) -> Result<String, IdentityError> {
        if !inv.token_digest.is_empty() {
            self.store
                .delete_record(kind::INVITE_TOKEN, &inv.token_digest)
                .await?;
        }
        let (secret, digest) = token()?;
        inv.token_digest = digest.clone();
        self.write_record(
            kind::INVITE_TOKEN,
            &digest,
            &inv.user,
            Some(inv.expires_at),
            &TokenOf {
                invitation: inv.id.clone(),
            },
        )
        .await?;
        self.write_record(kind::INVITATION, &inv.id, &inv.user, Some(inv.expires_at), &inv)
            .await?;
        Ok(secret)
    }

    /// Every invitation not yet accepted or cancelled, its link live or expired, oldest id first.
    pub async fn invitations(&self) -> Result<Vec<Invitation>, IdentityError> {
        Ok(self
            .store
            .records(kind::INVITATION, None)
            .await?
            .into_iter()
            .filter_map(|r| serde_json::from_str(&r.body).ok())
            .collect())
    }

    /// One invitation not yet accepted or cancelled, its link live or expired.
    pub async fn invitation(&self, id: &str) -> Result<Option<Invitation>, IdentityError> {
        Ok(self
            .store
            .record(kind::INVITATION, id)
            .await?
            .and_then(|r| serde_json::from_str(&r.body).ok()))
    }

    /// Removes an invitation and its token; true when it existed.
    pub async fn delete_invitation(&self, inv: &Invitation) -> Result<bool, IdentityError> {
        self.store
            .delete_record(kind::INVITE_TOKEN, &inv.token_digest)
            .await?;
        Ok(self.store.delete_record(kind::INVITATION, &inv.id).await?)
    }

    /// The invitation a token accepts, without using the token.
    pub async fn invitation_of_token(&self, secret: &str) -> Result<Option<Invitation>, IdentityError> {
        let digest = digest_hex(secret);
        let Some(rec) = self.live_record(kind::INVITE_TOKEN, &digest).await? else {
            return Ok(None);
        };
        let Ok(of) = serde_json::from_str::<TokenOf>(&rec.body) else {
            return Ok(None);
        };
        let now = self.now();
        Ok(self
            .invitation(&of.invitation)
            .await?
            .filter(|i| i.token_digest == digest && !i.expired(now)))
    }

    /// Uses an invitation token once: the token and its invitation are gone afterwards.
    pub async fn take_invitation(&self, secret: &str) -> Result<Option<Invitation>, IdentityError> {
        let Some(inv) = self.invitation_of_token(secret).await? else {
            return Ok(None);
        };
        let Some(taken) = self.take_live(kind::INVITE_TOKEN, &inv.token_digest).await? else {
            return Ok(None);
        };
        self.mark_used(kind::INVITE_TOKEN, &inv.token_digest, &inv.user, taken.expires_us)
            .await?;
        self.store.delete_record(kind::INVITATION, &inv.id).await?;
        Ok(Some(inv))
    }

    /// The pending invitation of a user, when it has one.
    pub async fn invitation_of_user(&self, user: &str) -> Result<Option<Invitation>, IdentityError> {
        let now = self.now();
        Ok(self
            .store
            .records(kind::INVITATION, Some(user))
            .await?
            .into_iter()
            .filter(|r| r.expires_us.is_none_or(|e| now.0 < e))
            .find_map(|r| serde_json::from_str(&r.body).ok()))
    }

    /// The user's preferences, `{}` when none were stored.
    pub async fn preferences(&self, user: &str) -> Result<Value, IdentityError> {
        Ok(self
            .read_record::<Value>(kind::PREFERENCES, user)
            .await?
            .map_or_else(|| Value::Object(serde_json::Map::new()), |(_, v)| v))
    }

    pub async fn set_preferences(&self, user: &str, prefs: &Value) -> Result<(), IdentityError> {
        self.write_record(kind::PREFERENCES, user, user, None, prefs)
            .await
    }

    /// A single-use token that confirms `email` as the user's new address, valid as long as a reset.
    pub async fn email_change_token(&self, user: &str, email: &str) -> Result<String, IdentityError> {
        let ttl = self.gens.load().identity.console.reset_ttl;
        let (secret, digest) = token()?;
        let expires = self.now().saturating_add(ttl);
        self.write_record(
            kind::EMAIL,
            &digest,
            user,
            Some(expires),
            &EmailChange {
                email: email.to_owned(),
            },
        )
        .await?;
        Ok(secret)
    }

    /// Uses an email-change token of `user`: the new address, once.
    pub async fn take_email_change(&self, user: &str, secret: &str) -> Result<Option<String>, IdentityError> {
        let digest = digest_hex(secret);
        let Some(rec) = self.live_record(kind::EMAIL, &digest).await? else {
            return Ok(None);
        };
        if rec.user != user {
            return Ok(None);
        }
        let Some(rec) = self.take_live(kind::EMAIL, &digest).await? else {
            return Ok(None);
        };
        Ok(serde_json::from_str::<EmailChange>(&rec.body)
            .ok()
            .map(|c| c.email))
    }

    /// Whether the user has a local password.
    pub async fn has_password(&self, user: &str) -> Result<bool, IdentityError> {
        Ok(self.password_of(user).await?.is_some())
    }

    /// Sets a password for a user who accepted an invitation (the policy applies).
    pub async fn set_initial_password(&self, user: &str, new: &str) -> Result<(), IdentityError> {
        self.set_password(user, new).await
    }

    /// The challenge an invited user continues with: enrolment of a second factor.
    pub async fn invitation_challenge(&self, user: &str) -> Result<super::LoginStep, IdentityError> {
        self.new_challenge(user, None, None).await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn an_invitation_expires_with_its_link_and_a_resend_renews_it() {
        let now = UtcMicros(1_000_000);
        let mut inv = Invitation::new(
            "ann@acme.example",
            "ann@acme.example",
            "admin",
            Value::Null,
            now,
            Duration::from_secs(60),
        )
        .unwrap();
        assert!(!inv.expired(now));
        let later = now.saturating_add(Duration::from_secs(60));
        assert!(inv.expired(later));
        inv.expires_at = later.saturating_add(Duration::from_secs(60));
        assert!(!inv.expired(later));
    }
}

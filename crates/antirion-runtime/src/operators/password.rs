//! Setting a local password: the policy (with history), changing it with the current one and a
//! fresh factor, and resetting it with a single-use token mailed to the user.

use antirion_identity::password::{self, Policy};
use antirion_identity::verifier::digest_hex;
use serde::{Deserialize, Serialize};

use super::signin::PasswordRecord;
use super::{IdentityError, LoginStep, Proof, kind, token};
use crate::Runtime;

#[derive(Debug, Serialize, Deserialize)]
struct ResetToken {
    user: String,
}

impl Runtime {
    /// Checks `new` against the policy and the user's history, then stores its hash.
    pub(crate) async fn set_password(&self, user: &str, new: &str) -> Result<(), IdentityError> {
        let snap = self.gens.load();
        let policy: Policy = snap.identity.console.policy.clone();
        let email = snap.identity.users.get(user).and_then(|u| u.email.clone());
        password::check(&policy, new, email.as_deref()).map_err(IdentityError::Policy)?;
        let previous = self.password_of(user).await?;
        let mut history: Vec<String> = previous
            .as_ref()
            .map(|p| {
                std::iter::once(p.hash.clone())
                    .chain(p.history.iter().cloned())
                    .collect()
            })
            .unwrap_or_default();
        let new_text = new.to_owned();
        let check_history = history.clone();
        let check_policy = policy.clone();
        let hashed = tokio::task::spawn_blocking(move || {
            if password::repeats(&check_policy, &new_text, &check_history) {
                return Err(IdentityError::Policy(password::Rule::History));
            }
            password::hash(&new_text).map_err(|_| IdentityError::Internal)
        })
        .await
        .map_err(|_| IdentityError::Internal)??;
        history.truncate(policy.history);
        let record = PasswordRecord {
            hash: hashed,
            history,
            changed_us: self.now().0,
        };
        self.write_record(kind::PASSWORD, user, user, None, &record).await
    }

    /// Changes the caller's password: the current password and a fresh second factor must verify.
    /// Every other session of the user ends; the one with digest `keep` stays.
    pub async fn change_password(
        &self,
        user: &str,
        current: &str,
        new: &str,
        proof: Option<&Proof>,
        keep: Option<&str>,
    ) -> Result<(), IdentityError> {
        let stored = self
            .password_of(user)
            .await?
            .ok_or(IdentityError::Unauthenticated)?;
        let presented = current.to_owned();
        let matches = tokio::task::spawn_blocking(move || password::verify(&presented, &stored.hash))
            .await
            .map_err(|_| IdentityError::Internal)?;
        if !matches {
            return Err(IdentityError::Unauthenticated);
        }
        let Some(proof) = proof else {
            return Err(IdentityError::Unauthenticated);
        };
        if !self.verify_proof(user, proof).await? {
            return Err(IdentityError::Unauthenticated);
        }
        self.set_password(user, new).await?;
        self.end_user_sessions(user, keep).await?;
        Ok(())
    }

    /// Whether password resets are offered: local accounts and mail are both enabled.
    #[must_use]
    pub fn resets_enabled(&self) -> bool {
        let snap = self.gens.load();
        snap.identity.console.local && snap.identity.console.mail.is_some()
    }

    /// A reset request: when `email` belongs to a user who may sign in with a password, mails it a
    /// single-use link under `link_base`. The answer never says which happened.
    pub async fn request_reset(
        &self,
        email: &str,
        link_base: &str,
        ip: Option<&str>,
    ) -> Result<(), IdentityError> {
        if !self.resets_enabled() {
            return Err(IdentityError::NotEnabled);
        }
        let snap = self.gens.load();
        let c = &snap.identity.console;
        let now = self.now();
        let keys = [
            ip.map(|ip| format!("reset-ip:{ip}")),
            Some(format!("reset-email:{}", email.trim().to_lowercase())),
        ];
        for key in keys.iter().flatten() {
            if let Some(retry_after) =
                self.state
                    .console
                    .throttle(key, now, c.throttle_failures, c.throttle_window)
            {
                return Err(IdentityError::Throttled { retry_after });
            }
        }
        let Some(user) = self.user_by_email(email) else {
            return Ok(());
        };
        if self.local_refusal(&user).is_some() {
            return Ok(());
        }
        let (secret, digest) = token()?;
        let expires = now.saturating_add(c.reset_ttl);
        self.write_record(
            kind::RESET,
            &digest,
            &user,
            Some(expires),
            &ResetToken { user: user.clone() },
        )
        .await?;
        let minutes = c.reset_ttl.as_secs() / 60;
        let body = format!(
            "A password reset was asked for the console account of {email}.\n\n\
             Set a new password here (the link works once, for {minutes} minutes):\n\
             {link_base}/reset-password?token={secret}\n\n\
             If you did not ask for it, ignore this message; your password stays the same.\n"
        );
        self.send_mail(email.trim(), "Reset your console password", body);
        Ok(())
    }

    /// Completes a reset: the token works once, the new password follows the policy, and every
    /// session and pending sign-in of the user ends. The next step is the second factor.
    pub async fn confirm_reset(&self, secret: &str, new: &str) -> Result<LoginStep, IdentityError> {
        if !self.resets_enabled() {
            return Err(IdentityError::NotEnabled);
        }
        let digest = digest_hex(secret);
        let rec = self
            .live_record(kind::RESET, &digest)
            .await?
            .ok_or(IdentityError::Invalid("the token is not valid"))?;
        let user = rec.user.clone();
        let snap = self.gens.load();
        let email = snap.identity.users.get(&user).and_then(|u| u.email.clone());
        password::check(&snap.identity.console.policy, new, email.as_deref())
            .map_err(IdentityError::Policy)?;
        let Some(taken) = self.take_live(kind::RESET, &digest).await? else {
            return Err(IdentityError::Invalid("the token is not valid"));
        };
        self.mark_used(kind::RESET, &digest, &user, taken.expires_us)
            .await?;
        self.set_password(&user, new).await?;
        self.void_challenges(&user).await?;
        self.end_user_sessions(&user, None).await?;
        self.new_challenge(&user, None, None).await
    }
}

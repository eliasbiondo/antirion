//! Read-only checks of the single-use tokens the console's public pages carry (an invitation, a
//! password reset): what the token's state is, without using it. A used token leaves a marker under
//! its digest, so a second visit to the same link can say it was used rather than unknown.

use antirion_core::UtcMicros;
use antirion_identity::rbac::Scope;
use antirion_identity::verifier::digest_hex;
use serde_json::Value;

use super::{IdentityError, Invitation, kind, organization_of};
use crate::Runtime;

/// What a token check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenState {
    Valid,
    Expired,
    Used,
    Unknown,
}

impl TokenState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TokenState::Valid => "valid",
            TokenState::Expired => "expired",
            TokenState::Used => "used",
            TokenState::Unknown => "unknown",
        }
    }
}

/// A checked invitation token: the invitation itself only while the token is valid.
#[derive(Clone, Debug)]
pub struct InvitationCheck {
    pub state: TokenState,
    pub invitation: Option<Invitation>,
    /// The organization the invitation's first organization-bound role binding belongs to.
    pub organization: Option<String>,
}

/// A checked reset token: when it stops working, only while it is valid.
#[derive(Clone, Copy, Debug)]
pub struct ResetCheck {
    pub state: TokenState,
    pub expires_at: Option<UtcMicros>,
}

fn used_id(kind: &str, digest: &str) -> String {
    format!("{kind}:{digest}")
}

/// The organization of the first binding whose scope belongs to one.
fn invitation_organization(rt: &Runtime, inv: &Invitation) -> Option<String> {
    let snap = rt.gens.load();
    inv.role_bindings
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b.get("scope").and_then(Value::as_str))
        .filter_map(Scope::parse)
        .find_map(|s| organization_of(&snap.identity, &s))
}

impl Runtime {
    /// Leaves the marker of a used token of `token_kind`, kept as long as the token would have
    /// lived and deleted with its user.
    pub(crate) async fn mark_used(
        &self,
        token_kind: &str,
        digest: &str,
        user: &str,
        expires: Option<i64>,
    ) -> Result<(), IdentityError> {
        self.write_record(
            kind::USED_TOKEN,
            &used_id(token_kind, digest),
            user,
            expires.map(UtcMicros),
            &Value::Null,
        )
        .await
    }

    /// The state of a stored token record of `token_kind`, by digest; the record when valid.
    async fn token_state(
        &self,
        token_kind: &str,
        digest: &str,
    ) -> Result<(TokenState, Option<antirion_store::IdentityRecord>), IdentityError> {
        if let Some(rec) = self.store.record(token_kind, digest).await? {
            let live = rec.expires_us.is_none_or(|e| self.now().0 < e);
            return Ok(if live {
                (TokenState::Valid, Some(rec))
            } else {
                (TokenState::Expired, None)
            });
        }
        let used = self
            .store
            .record(kind::USED_TOKEN, &used_id(token_kind, digest))
            .await?
            .is_some();
        Ok((
            if used {
                TokenState::Used
            } else {
                TokenState::Unknown
            },
            None,
        ))
    }

    /// Checks an invitation token without using it. A token voided by a resend or a cancelled
    /// invitation is unknown.
    pub async fn check_invitation(&self, secret: &str) -> Result<InvitationCheck, IdentityError> {
        let digest = digest_hex(secret);
        let (state, _) = self.token_state(kind::INVITE_TOKEN, &digest).await?;
        let unknown = InvitationCheck {
            state: TokenState::Unknown,
            invitation: None,
            organization: None,
        };
        match state {
            TokenState::Valid => {
                let Some(inv) = self.invitation_of_token(secret).await? else {
                    return Ok(unknown);
                };
                let organization = invitation_organization(self, &inv);
                Ok(InvitationCheck {
                    state,
                    invitation: Some(inv),
                    organization,
                })
            }
            _ => Ok(InvitationCheck { state, ..unknown }),
        }
    }

    /// Checks a password-reset token without using it. The answer names no account: a token is
    /// only ever issued to one, and the caller already holds it.
    pub async fn check_reset(&self, secret: &str) -> Result<ResetCheck, IdentityError> {
        if !self.resets_enabled() {
            return Err(IdentityError::NotEnabled);
        }
        let (state, rec) = self.token_state(kind::RESET, &digest_hex(secret)).await?;
        Ok(ResetCheck {
            state,
            expires_at: rec.and_then(|r| r.expires_us).map(UtcMicros),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_have_their_wire_names() {
        let names: Vec<&str> = [
            TokenState::Valid,
            TokenState::Expired,
            TokenState::Used,
            TokenState::Unknown,
        ]
        .iter()
        .map(|s| s.as_str())
        .collect();
        assert_eq!(names, ["valid", "expired", "used", "unknown"]);
    }

    #[test]
    fn markers_are_kept_per_token_kind() {
        assert_ne!(used_id("reset", "d"), used_id("invite_token", "d"));
    }
}

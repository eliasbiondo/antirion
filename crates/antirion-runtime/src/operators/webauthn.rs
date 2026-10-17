//! WebAuthn second factors (Level 3, user verification required), through `webauthn_rp`:
//! registration of a credential, and an assertion against the credentials of a user at sign-in.
//! Ceremony state is kept in the store between the two calls, so any node can finish what another
//! started; the relying party id is `local.mfa.webauthn.rp_id`, or the host of the console's origin.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use webauthn_rp::bin::{Decode, Encode};
use webauthn_rp::request::auth::{AllowedCredential, AllowedCredentials, AuthenticationVerificationOptions};
use webauthn_rp::request::register::{
    PublicKeyCredentialUserEntity, RegistrationVerificationOptions, UserHandle16,
};
use webauthn_rp::request::{
    AsciiDomain, Credentials, PublicKeyCredentialDescriptor, RpId, UserVerificationRequirement,
};
use webauthn_rp::response::register::{DynamicState, StaticState};
use webauthn_rp::response::{AuthTransports, CredentialId};
use webauthn_rp::{
    AuthenticatedCredential, NonDiscoverableAuthentication16, NonDiscoverableAuthenticationServerState,
    NonDiscoverableCredentialRequestOptions, PublicKeyCredentialCreationOptions, Registration,
    RegistrationServerState,
};

use super::factors::FactorRecord;
use super::{FactorKind, IdentityError, kind};
use crate::Runtime;

/// The user handle of an account: random, the same for all its credentials.
#[derive(Debug, Serialize, Deserialize)]
struct Handle {
    handle: String,
}

/// A registered credential as stored, each part in the library's binary form.
#[derive(Debug, Serialize, Deserialize)]
struct Stored {
    credential_id: String,
    #[serde(rename = "static")]
    static_state: String,
    dynamic: String,
    #[serde(default)]
    transports: u8,
}

/// The relying party id: the configured one, else the host of `origin`.
fn rp_id(configured: Option<&str>, origin: &str) -> Option<RpId> {
    let host = match configured {
        Some(id) => id.to_owned(),
        None => {
            let rest = origin.split_once("://").map_or(origin, |(_, r)| r);
            let authority = rest.split('/').next().unwrap_or(rest);
            if authority.starts_with('[') {
                return None;
            }
            authority
                .rsplit_once(':')
                .map_or(authority, |(h, _)| h)
                .to_owned()
        }
    };
    AsciiDomain::try_from(host).ok().map(RpId::Domain)
}

fn decode(text: &str) -> Result<Vec<u8>, IdentityError> {
    B64.decode(text).map_err(|_| IdentityError::Internal)
}

fn handle_of(bytes: &[u8]) -> Result<UserHandle16, IdentityError> {
    let fixed: [u8; 16] = bytes.try_into().map_err(|_| IdentityError::Internal)?;
    UserHandle16::decode(fixed).map_err(|_| IdentityError::Internal)
}

impl Runtime {
    fn webauthn_rp(&self, origin: &str) -> Result<RpId, IdentityError> {
        let snap = self.gens.load();
        if !snap.identity.console.allows(FactorKind::Webauthn) {
            return Err(IdentityError::Invalid("webauthn is not an allowed factor"));
        }
        rp_id(snap.identity.console.rp_id.as_deref(), origin).ok_or(IdentityError::Invalid(
            "no relying party id: set local.mfa.webauthn.rp_id",
        ))
    }

    async fn user_handle(&self, user: &str) -> Result<UserHandle16, IdentityError> {
        if let Some((_, h)) = self.read_record::<Handle>(kind::WEBAUTHN_HANDLE, user).await? {
            return handle_of(&decode(&h.handle)?);
        }
        let handle = UserHandle16::new();
        let bytes = handle.encode().map_err(|_| IdentityError::Internal)?;
        let body = Handle {
            handle: B64.encode(bytes),
        };
        self.write_record(kind::WEBAUTHN_HANDLE, user, user, None, &body)
            .await?;
        Ok(handle)
    }

    /// Starts registering a WebAuthn credential: the factor id and the creation options for the
    /// browser (`PublicKeyCredentialCreationOptionsJSON`).
    pub async fn enrol_webauthn(&self, user: &str, origin: &str) -> Result<(String, Value), IdentityError> {
        let rp = self.webauthn_rp(origin)?;
        let handle = self.user_handle(user).await?;
        let snap = self.gens.load();
        let name = snap
            .identity
            .users
            .get(user)
            .and_then(|u| u.email.clone())
            .unwrap_or_else(|| user.to_owned());
        let entity = PublicKeyCredentialUserEntity {
            name: name
                .as_str()
                .try_into()
                .map_err(|_| IdentityError::Invalid("the user name cannot name a credential"))?,
            id: &handle,
            display_name: None,
        };
        let mut options = PublicKeyCredentialCreationOptions::second_factor(&rp, entity, Vec::new());
        options.authenticator_selection.user_verification = UserVerificationRequirement::Required;
        let (server, client) = options.start_ceremony().map_err(|_| IdentityError::Internal)?;
        let state = server.encode().map_err(|_| IdentityError::Internal)?;
        let client_json = serde_json::to_value(&client).map_err(|_| IdentityError::Internal)?;
        let id = super::factors::new_factor_id()?;
        let record = FactorRecord {
            kind: "webauthn".to_owned(),
            name: None,
            confirmed: false,
            created_us: self.now().0,
            last_used_us: None,
            secret: None,
            last_step: None,
            webauthn: Some(serde_json::json!({"state": B64.encode(state)})),
        };
        self.write_record(kind::FACTOR, &id, user, None, &record).await?;
        Ok((id, client_json))
    }

    /// Verifies the browser's registration of factor `id` and confirms it; the recovery codes of a
    /// first factor.
    pub async fn confirm_webauthn(
        &self,
        user: &str,
        id: &str,
        response: &Value,
        origin: &str,
        name: Option<String>,
    ) -> Result<Vec<String>, IdentityError> {
        let rp = self.webauthn_rp(origin)?;
        let Some((rec, f)) = self.read_record::<FactorRecord>(kind::FACTOR, id).await? else {
            return Err(IdentityError::NotFound);
        };
        if rec.user != user || f.kind != "webauthn" {
            return Err(IdentityError::NotFound);
        }
        if f.confirmed {
            return Err(IdentityError::Conflict("the factor is already confirmed"));
        }
        let state = f
            .webauthn
            .as_ref()
            .and_then(|w| w.get("state"))
            .and_then(Value::as_str)
            .ok_or(IdentityError::NotFound)?;
        let state = decode(state)?;
        let server = RegistrationServerState::<16>::decode(state.as_slice())
            .map_err(|_| IdentityError::Invalid("the registration is not valid"))?;
        let body = serde_json::to_vec(response).map_err(|_| IdentityError::Internal)?;
        let registration = Registration::from_json_relaxed(&body)
            .map_err(|_| IdentityError::Invalid("the registration is not valid"))?;
        let origins = [origin];
        let options = RegistrationVerificationOptions::<&str, &str> {
            allowed_origins: &origins,
            ..RegistrationVerificationOptions::default()
        };
        let credential = server
            .verify(&rp, &registration, &options)
            .map_err(|_| IdentityError::Unauthenticated)?;
        let static_state = credential
            .static_state()
            .encode()
            .map_err(|_| IdentityError::Internal)?;
        let dynamic = credential
            .dynamic_state()
            .encode()
            .map_err(|_| IdentityError::Internal)?;
        let id_of = credential.id();
        let credential_id = id_of.encode().map_err(|_| IdentityError::Internal)?;
        let stored = Stored {
            credential_id: B64.encode(credential_id),
            static_state: B64.encode(static_state),
            dynamic: B64.encode(dynamic),
            transports: credential
                .transports()
                .encode()
                .map_err(|_| IdentityError::Internal)?,
        };
        let updated = FactorRecord {
            webauthn: Some(serde_json::to_value(&stored).map_err(|_| IdentityError::Internal)?),
            ..f
        };
        self.write_record(kind::FACTOR, id, user, None, &updated).await?;
        self.mark_confirmed(user, id, name).await
    }

    /// Starts an assertion against the user's WebAuthn credentials: the server state to keep with the
    /// sign-in challenge, and the request options for the browser. `None` when it has none.
    pub(crate) async fn start_assertion(
        &self,
        user: &str,
        origin: &str,
    ) -> Result<Option<(String, Value)>, IdentityError> {
        let Ok(rp) = self.webauthn_rp(origin) else {
            return Ok(None);
        };
        let mut allowed = AllowedCredentials::with_capacity(1);
        for (_, f) in self.confirmed_factors(user).await? {
            let Some(stored) = f.webauthn.and_then(|w| serde_json::from_value::<Stored>(w).ok()) else {
                continue;
            };
            let id = CredentialId::<Vec<u8>>::decode(decode(&stored.credential_id)?)
                .map_err(|_| IdentityError::Internal)?;
            let transports =
                AuthTransports::decode(stored.transports).map_err(|_| IdentityError::Internal)?;
            allowed.push(AllowedCredential::from(PublicKeyCredentialDescriptor {
                id,
                transports,
            }));
        }
        if allowed.is_empty() {
            return Ok(None);
        }
        let mut options = NonDiscoverableCredentialRequestOptions::second_factor(&rp, allowed)
            .map_err(|_| IdentityError::Internal)?;
        options.options().user_verification = UserVerificationRequirement::Required;
        let (server, client) = options.start_ceremony().map_err(|_| IdentityError::Internal)?;
        let state = server.encode().map_err(|_| IdentityError::Internal)?;
        let client_json = serde_json::to_value(&client).map_err(|_| IdentityError::Internal)?;
        Ok(Some((B64.encode(state), client_json)))
    }

    /// Verifies an assertion against the state started with the challenge; the credential's
    /// signature counter is updated.
    pub(crate) async fn verify_assertion(
        &self,
        user: &str,
        state: &str,
        assertion: &Value,
        origin: &str,
    ) -> Result<bool, IdentityError> {
        let rp = self.webauthn_rp(origin)?;
        let state = decode(state)?;
        let Ok(server) = NonDiscoverableAuthenticationServerState::decode(state.as_slice()) else {
            return Ok(false);
        };
        let body = serde_json::to_vec(assertion).map_err(|_| IdentityError::Internal)?;
        let Ok(authentication) = NonDiscoverableAuthentication16::from_json_relaxed(&body) else {
            return Ok(false);
        };
        let raw_id = authentication.raw_id();
        let raw: &[u8] = raw_id.encode().map_err(|_| IdentityError::Internal)?;
        for (id, f) in self.confirmed_factors(user).await? {
            let Some(stored) = f
                .webauthn
                .clone()
                .and_then(|w| serde_json::from_value::<Stored>(w).ok())
            else {
                continue;
            };
            let cred_bytes = decode(&stored.credential_id)?;
            if cred_bytes.as_slice() != raw {
                continue;
            }
            let handle = self.user_handle(user).await?;
            let static_bytes = decode(&stored.static_state)?;
            let static_state =
                StaticState::decode(static_bytes.as_slice()).map_err(|_| IdentityError::Internal)?;
            let dynamic_bytes: [u8; 7] = decode(&stored.dynamic)?
                .as_slice()
                .try_into()
                .map_err(|_| IdentityError::Internal)?;
            let dynamic = DynamicState::decode(dynamic_bytes).map_err(|_| IdentityError::Internal)?;
            let cred_id =
                CredentialId::<&[u8]>::decode(cred_bytes.as_slice()).map_err(|_| IdentityError::Internal)?;
            let mut cred = AuthenticatedCredential::new(cred_id, &handle, static_state, dynamic)
                .map_err(|_| IdentityError::Internal)?;
            let origins = [origin];
            let options = AuthenticationVerificationOptions::<&str, &str> {
                allowed_origins: &origins,
                ..AuthenticationVerificationOptions::default()
            };
            if server.verify(&rp, &authentication, &mut cred, &options).is_err() {
                return Ok(false);
            }
            let dynamic = cred
                .dynamic_state()
                .encode()
                .map_err(|_| IdentityError::Internal)?;
            let updated = FactorRecord {
                webauthn: Some(
                    serde_json::to_value(Stored {
                        dynamic: B64.encode(dynamic),
                        ..stored
                    })
                    .map_err(|_| IdentityError::Internal)?,
                ),
                last_used_us: Some(self.now().0),
                ..f
            };
            self.write_record(kind::FACTOR, &id, user, None, &updated).await?;
            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relying_party_is_the_configured_id_or_the_origin_host() {
        let id = |c: Option<&str>, o: &str| rp_id(c, o).map(|r| r.as_ref().to_owned());
        assert_eq!(
            id(None, "https://gw.acme.example:8443").as_deref(),
            Some("gw.acme.example")
        );
        assert_eq!(id(None, "http://localhost:20581").as_deref(), Some("localhost"));
        assert_eq!(
            id(Some("acme.example"), "https://gw.acme.example").as_deref(),
            Some("acme.example")
        );
        assert!(id(None, "http://[::1]:8081").is_none());
    }
}

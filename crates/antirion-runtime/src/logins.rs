//! Single sign-on logins between their start and their callback. In cluster mode they live in the
//! store, so the callback may reach any node.

use std::time::Duration;

use antirion_core::UtcMicros;
use serde_json::{Value, json};

use crate::{OidcLogin, Runtime};

fn to_json(l: &OidcLogin) -> String {
    json!({
        "provider": l.provider,
        "nonce": l.nonce,
        "verifier": l.verifier,
        "redirect_uri": l.redirect_uri,
        "return_to": l.return_to,
        "started": l.started.0,
        "test": l.test,
        "trust_device": l.trust_device,
    })
    .to_string()
}

fn from_json(body: &str) -> Option<OidcLogin> {
    let v: Value = serde_json::from_str(body).ok()?;
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    Some(OidcLogin {
        provider: text("provider")?,
        nonce: text("nonce")?,
        verifier: text("verifier")?,
        redirect_uri: text("redirect_uri")?,
        return_to: text("return_to")?,
        started: UtcMicros(v.get("started").and_then(Value::as_i64)?),
        test: v.get("test").and_then(Value::as_bool).unwrap_or(false),
        trust_device: v.get("trust_device").and_then(Value::as_bool).unwrap_or(false),
    })
}

impl Runtime {
    /// Records a login in progress under its `state`; logins older than `lifetime` are dropped.
    pub async fn start_login(&self, state: String, login: OidcLogin, lifetime: Duration) -> bool {
        if let Some(pg) = self.store.postgres() {
            return match pg.put_login(&state, &to_json(&login), login.started.0).await {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(error = %e, "a login could not be recorded");
                    false
                }
            };
        }
        let now = self.now();
        let mut logins = self.state.oidc_logins.lock();
        logins.retain(|_, l| now.since(l.started) <= lifetime);
        logins.insert(state, login);
        true
    }

    /// The login started under `state`, left in place: a response is checked against it before it
    /// is taken, so one that fails the checks cannot end the login it names.
    pub async fn pending_login(&self, state: &str) -> Option<OidcLogin> {
        match self.store.postgres() {
            Some(pg) => pg
                .login(state)
                .await
                .ok()
                .flatten()
                .as_deref()
                .and_then(from_json),
            None => self.state.oidc_logins.lock().get(state).cloned(),
        }
    }

    /// Removes and returns the login started under `state`.
    pub async fn take_login(&self, state: &str) -> Option<OidcLogin> {
        match self.store.postgres() {
            Some(pg) => pg
                .take_login(state)
                .await
                .ok()
                .flatten()
                .as_deref()
                .and_then(from_json),
            None => self.state.oidc_logins.lock().remove(state),
        }
    }
}

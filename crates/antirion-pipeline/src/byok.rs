//! Client provider keys (BYOK): the key a client brings in `x-antirion-provider-key`, or in a
//! provider's own credential header beside a gateway credential in `x-antirion-api-key`, honoured
//! only for keys with the `byok` permission and only on deployments whose credential mode accepts
//! it.

use antirion_core::Stage;
use antirion_error::{GatewayError, Message};
use antirion_identity::keyfmt::Credential;
use antirion_identity::permissions::KeyPermission;
use antirion_providers::{Byok, DeploymentRt};
use antirion_runtime::identity::KeyRt;
use http::HeaderMap;

const PROVIDER_KEY: &str = "x-antirion-provider-key";
const GATEWAY_KEY: &str = "x-antirion-api-key";

/// The provider credential a request brings.
#[derive(Clone, Debug, Default)]
pub(crate) struct ClientKeys {
    explicit: Option<String>,
    /// The provider credential headers present beside `x-antirion-api-key`.
    native: HeaderMap,
}

impl ClientKeys {
    /// The client's provider credentials, when its key may use them.
    pub(crate) fn of(headers: &HeaderMap, key: &KeyRt) -> Option<ClientKeys> {
        if !key.permissions.has(KeyPermission::Byok) {
            return None;
        }
        let text = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let explicit = text(PROVIDER_KEY);
        let mut native = HeaderMap::new();
        if headers.contains_key(GATEWAY_KEY) {
            for name in ["authorization", "x-api-key", "api-key", "x-goog-api-key"] {
                if let Some(v) = headers.get(name) {
                    native.insert(name, v.clone());
                }
            }
        }
        (explicit.is_some() || !native.is_empty()).then_some(ClientKeys { explicit, native })
    }

    /// The client's key for `d`: the explicit one, else the value of the header the provider's key
    /// travels in, without its prefix. A well-formed gateway key is never a provider credential.
    pub(crate) fn for_deployment(&self, d: &DeploymentRt) -> Option<String> {
        if let Some(k) = &self.explicit {
            return Some(k.clone());
        }
        let (name, prefix) = d.provider.key_header.as_ref()?;
        let raw = self.native.get(name)?.to_str().ok()?.trim();
        let value = match prefix.trim() {
            "" => raw,
            p => {
                let (scheme, rest) = raw.split_once(' ')?;
                if !scheme.eq_ignore_ascii_case(p) {
                    return None;
                }
                rest.trim()
            }
        };
        (!value.is_empty() && Credential::parse(value).is_none()).then(|| value.to_owned())
    }
}

/// The exclusion of a deployment that requires a client key the request does not bring.
pub(crate) const KEY_REQUIRED: &str = "byok";

/// 401 `unauthenticated`, reason `provider_key_required`: no deployment is left without the
/// client's own provider key.
pub(crate) fn key_required() -> GatewayError {
    GatewayError::with_reason(
        crate::reasons::PROVIDER_KEY_REQUIRED,
        Stage::Route,
        Message::Fixed("A provider key is required: send your own key in x-antirion-provider-key."),
    )
}

/// Whether a deployment can serve a request with these client keys: one that requires a client key
/// needs one, and a request that brings one goes only to deployments that accept it.
pub(crate) fn eligible(keys: Option<&ClientKeys>, d: &DeploymentRt) -> Result<(), Ineligible> {
    let brought = keys.and_then(|k| k.for_deployment(d));
    match (d.byok, brought) {
        (Byok::Required, None) => Err(Ineligible::KeyRequired),
        (Byok::Disabled, Some(_)) => Err(Ineligible::NotAccepted),
        _ => Ok(()),
    }
}

/// Why a deployment cannot serve a request because of its credential mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ineligible {
    /// `byok: required` and the client brought no key.
    KeyRequired,
    /// The client brought a key and the deployment does not accept one.
    NotAccepted,
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use super::*;

    fn keys(pairs: &[(&'static str, &str)]) -> ClientKeys {
        let mut native = HeaderMap::new();
        for (k, v) in pairs {
            native.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        ClientKeys {
            explicit: None,
            native,
        }
    }

    fn deployment(provider: serde_json::Value) -> DeploymentRt {
        let doc = serde_json::json!({"providers": {"p": provider},
                                     "deployments": {"d": {"provider": "p", "model": "m"}}});
        let t = antirion_providers::compile(&doc).unwrap();
        (*t.deployments["d"]).clone()
    }

    #[test]
    fn native_headers_lose_their_prefix_and_gateway_keys_are_never_used() {
        let openai = deployment(
            serde_json::json!({"type": "openai", "byok": "allowed", "auth": {"api_key": "sk-op"}}),
        );
        let anthropic =
            deployment(serde_json::json!({"type": "anthropic", "byok": "allowed", "auth": {"api_key": "k"}}));
        let k = keys(&[("authorization", "Bearer sk-client"), ("x-api-key", "ant-client")]);
        assert_eq!(k.for_deployment(&openai).as_deref(), Some("sk-client"));
        assert_eq!(k.for_deployment(&anthropic).as_deref(), Some("ant-client"));
        let gateway = Credential::generate(antirion_identity::keyfmt::Plane::Data).unwrap();
        let g = keys(&[("authorization", &format!("Bearer {}", gateway.expose()))]);
        assert_eq!(g.for_deployment(&openai), None);
    }

    #[test]
    fn credential_modes_decide_eligibility() {
        let required =
            deployment(serde_json::json!({"type": "openai", "byok": "required", "auth": {"api_key": "k"}}));
        let disabled = deployment(serde_json::json!({"type": "openai", "auth": {"api_key": "k"}}));
        let brought = ClientKeys {
            explicit: Some("sk-c".into()),
            native: HeaderMap::new(),
        };
        assert_eq!(eligible(None, &required), Err(Ineligible::KeyRequired));
        assert_eq!(eligible(Some(&brought), &required), Ok(()));
        assert_eq!(eligible(Some(&brought), &disabled), Err(Ineligible::NotAccepted));
        assert_eq!(eligible(None, &disabled), Ok(()));
    }
}

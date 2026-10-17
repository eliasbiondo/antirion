//! `GET /runtime/encryption`: what protects data in transit and at rest on the serving node. The
//! listeners' TLS with the certificate each serves, and the encryption of stored secrets with the
//! ids of its keys; never a key, and never a private part of a certificate.

use antirion_identity::rbac::Scope;
use antirion_runtime::security::{ClientAuth, ListenerTls};
use axum::response::Response;
use http::{Method, StatusCode};
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::call::Call;
use crate::danger_routes::only;
use crate::error::AdminError;

/// The certificate facts of a PEM chain: its leaf, and how many certificates it holds.
fn certificate(pem: &[u8]) -> Value {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(pem)
        .filter_map(Result::ok)
        .collect();
    let Some(leaf) = chain.first() else {
        return Value::Null;
    };
    let Some(facts) = crate::certinfo::read(leaf) else {
        return Value::Null;
    };
    let sha256: String = Sha256::digest(leaf.as_ref())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    json!({
        "subject": facts.subject,
        "issuer": facts.issuer,
        "dns_names": facts.dns_names,
        "not_before": facts.not_before.to_rfc3339(),
        "not_after": facts.not_after.to_rfc3339(),
        "sha256": sha256,
        "chain_length": chain.len(),
    })
}

async fn listener(tls: Option<&ListenerTls>) -> Value {
    let Some(l) = tls else {
        return json!({"enabled": false, "versions": [], "client_auth": "none", "certificate": null});
    };
    let versions: &[&str] = if l.min_tls13 { &["1.3"] } else { &["1.2", "1.3"] };
    let client_auth = match l.client_auth {
        ClientAuth::None => "none",
        ClientAuth::Optional => "optional",
        ClientAuth::Required => "required",
    };
    let cert = match tokio::fs::read(&l.cert_file).await {
        Ok(pem) => certificate(&pem),
        Err(_) => Value::Null,
    };
    json!({"enabled": true, "versions": versions, "client_auth": client_auth, "certificate": cert})
}

pub(crate) async fn report(call: &Call) -> Result<Response, AdminError> {
    only(call, Method::GET, "GET")?;
    call.allow_params(&[])?;
    call.require("security:read", &[Scope::Gateway])?;
    let security = &call.snap.security;
    let keys: Vec<Value> = call
        .rt
        .sealer
        .key_ids()
        .into_iter()
        .enumerate()
        .map(|(i, (id, configured))| {
            json!({"id": id, "primary": i == 0,
                   "source": if configured { "configured" } else { "data_dir" }})
        })
        .collect();
    let stores = external_stores(call);
    let body = json!({
        "inbound_tls": {
            "data": listener(security.tls_data.as_ref()).await,
            "admin": listener(security.tls_admin.as_ref()).await,
            "reload_interval": antirion_config::scalar::format_duration(security.tls_reload),
        },
        "at_rest": {
            "algorithm": "AES-256-GCM",
            "keys": keys,
            "external_stores": stores,
        },
    });
    Ok(call.json(StatusCode::OK, &body))
}

/// Each configured store: how many references name it and what reading them last did.
fn external_stores(call: &Call) -> Vec<Value> {
    let doc = &call.snap.config.doc;
    let refs = antirion_config::secrets::store_references(doc);
    let status = call.rt.secret_stores.status();
    doc.pointer("/security/secrets/stores")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(name, _)| {
            let scheme = name.replace('_', "-");
            let references = refs.iter().filter(|(s, _)| *s == scheme).count();
            let st = status.get(name.as_str()).cloned().unwrap_or_default();
            let state = if references == 0 {
                "unused"
            } else if st.failing {
                "failing"
            } else {
                "ok"
            };
            json!({
                "name": name,
                "references": references,
                "state": state,
                "last_refresh_at": st.last_refresh_at.map(antirion_core::UtcMicros::to_rfc3339_seconds),
                "last_error": st.last_error,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEM: &str = "-----BEGIN CERTIFICATE-----
MIIBljCCATygAwIBAgIUQcRVs7N3dXgJIGNQf0KYqdxuCNcwCgYIKoZIzj0EAwIw
GjEYMBYGA1UEAwwPZ3cuYWNtZS5leGFtcGxlMB4XDTI2MTAxNDE2MDA0NloXDTI2
MTExMzE2MDA0NlowGjEYMBYGA1UEAwwPZ3cuYWNtZS5leGFtcGxlMFkwEwYHKoZI
zj0CAQYIKoZIzj0DAQcDQgAEUuCO0TL5fbk9Ka/JywmwrfAn+lgQpW6qC35TtAvW
6eayawQ6NPFVfejSAAAY5DhQXFhnd9I4/n6oLvbYA/JQjKNgMF4wHQYDVR0OBBYE
FEjth2e8F2WrqUCy5yRvoEW1i4PfMA8GA1UdEwEB/wQFMAMBAf8wLAYDVR0RBCUw
I4IPZ3cuYWNtZS5leGFtcGxlghBhbHQuYWNtZS5leGFtcGxlMAoGCCqGSM49BAMC
A0gAMEUCID6HWH39/k9lAq7ZX8HXC5ck4OYULQ42B53MprqYNKpHAiEArNxn5u8B
x9B7zeTyeCZwGf2oN1BCIhFk19vCz9EFhyI=
-----END CERTIFICATE-----
";

    #[test]
    fn a_pem_chain_shows_its_leaf() {
        let v = certificate(PEM.as_bytes());
        assert_eq!(v["subject"], "gw.acme.example");
        assert_eq!(v["chain_length"], 1);
        assert_eq!(
            v["sha256"],
            "bf0edb868d67537736ca5e331cf1ec2ce48b0f748ed9e5278537cdf1d137abbf"
        );
        assert!(certificate(b"not a certificate").is_null());
    }
}

//! The client certificate a request presents, for keys bound to one: the certificate the
//! listener verified, or, behind a trusted proxy that is allowed to pass it, the one in
//! `Client-Cert`.

use std::sync::Arc;

use antirion_core::{ClientCert, Peer};
use antirion_identity::certificate;
use antirion_runtime::identity::KeyRt;
use http::request::Parts;

use crate::ctx::Ctx;

/// RFC 9440's header for the certificate a TLS-terminating proxy verified.
pub(crate) const CLIENT_CERT: &str = "client-cert";

fn presented(cx: &Ctx, parts: &Parts) -> Option<Arc<ClientCert>> {
    let peer = parts.extensions.get::<Peer>()?;
    if let Some(c) = &peer.client_cert {
        return Some(Arc::clone(c));
    }
    if !cx.snap.identity.settings.forwarded_client_cert
        || !cx.snap.security.client_ip.trusts_peer(peer.addr.ip())
    {
        return None;
    }
    let der = certificate::from_header(parts.headers.get(CLIENT_CERT)?.as_bytes())?;
    Some(Arc::new(certificate::client_cert(&der)))
}

/// Whether the request may use `key`: always, unless the key is bound to a certificate the
/// request does not present.
pub(crate) fn satisfied(cx: &Ctx, parts: &Parts, key: &KeyRt) -> bool {
    let Some(binding) = &key.client_cert else {
        return true;
    };
    presented(cx, parts).is_some_and(|c| binding.matches(&c))
}

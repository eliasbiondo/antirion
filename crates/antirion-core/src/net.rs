//! Connection facts the listeners attach to each request.

use std::net::SocketAddr;
use std::sync::Arc;

/// The connection a request arrived on, set on each request by the listener (data and admin).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// The TCP peer's address (before any trusted-proxy resolution).
    pub addr: SocketAddr,
    /// The listener terminated TLS.
    pub tls: bool,
    /// The client certificate the listener verified (mutual TLS).
    pub client_cert: Option<Arc<ClientCert>>,
}

/// What authentication reads of a verified client certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientCert {
    /// The SHA-256 of the leaf certificate's DER.
    pub sha256: [u8; 32],
    /// The leaf's subjectAltName entries: DNS names, URIs, email addresses and IP addresses as text.
    pub names: Vec<String>,
}

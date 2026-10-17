//! Outgoing email: a small SMTP client (RFC 5321) for one message at a time, with STARTTLS (RFC
//! 3207), implicit TLS (RFC 8314), `AUTH PLAIN` and `AUTH LOGIN`, and the message format of RFC 5322
//! with a UTF-8 text body.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

mod message;
mod smtp;

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

pub use message::{Message, MessageError};
pub use smtp::send;

/// How the connection to the server is protected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    /// Plain TCP upgraded with STARTTLS, which the server must offer.
    StartTls,
    /// TLS from the first byte.
    Tls,
    /// No TLS at all (callers allow it only for a loopback host).
    None,
}

/// Where and how to send.
#[derive(Clone)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Bounds the connection and every command's answer.
    pub timeout: Duration,
    /// The name this client gives in `EHLO`.
    pub helo_name: String,
    /// The certificates the server's chain is verified against.
    pub tls: Arc<rustls::ClientConfig>,
    /// Which of the host's addresses may be connected to: the caller's egress rules. Without it
    /// any address is.
    pub allowed: Option<AddrGuard>,
}

/// Decides whether one resolved address of the mail server may be connected to.
pub type AddrGuard = Arc<dyn Fn(IpAddr) -> bool + Send + Sync>;

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("security", &self.security)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// The TLS settings of a mail connection: the bundled web PKI roots, TLS 1.2 or later.
#[must_use]
pub fn default_tls() -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map(|b| b.with_root_certificates(roots.clone()).with_no_client_auth())
        .unwrap_or_else(|_| {
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        });
    Arc::new(config)
}

/// Why a message was not delivered to the server.
#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("cannot connect to the mail server: {0}")]
    Connect(String),
    #[error("the mail server did not answer in time")]
    Timeout,
    #[error("the mail server does not offer STARTTLS")]
    NoStartTls,
    #[error("the TLS handshake with the mail server failed: {0}")]
    Tls(String),
    #[error("the mail server refused authentication")]
    Auth,
    #[error("the mail server answered {code} to {command}")]
    Refused { command: &'static str, code: u16 },
    #[error("the mail server's answer cannot be read")]
    Protocol,
    #[error(transparent)]
    Message(#[from] MessageError),
}

impl MailError {
    /// Whether trying again later can succeed (a temporary or connection failure).
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            MailError::Connect(_) | MailError::Timeout | MailError::Protocol => true,
            MailError::Refused { code, .. } => (400..500).contains(code),
            MailError::NoStartTls | MailError::Tls(_) | MailError::Auth | MailError::Message(_) => false,
        }
    }
}

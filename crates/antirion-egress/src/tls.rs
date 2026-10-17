//! Upstream connection profiles: the bundled web PKI roots plus an optional CA bundle, hostname
//! verification against the URL host or a configured name, TLS 1.2 or later, and the HTTP version
//! (ALPN `h2`, `http/1.1` unless one version is required; HTTP/2 with prior knowledge in cleartext).

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};

/// The HTTP version spoken to a destination.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum HttpVersion {
    /// HTTP/2 when TLS negotiates it through ALPN, else HTTP/1.1; cleartext follows the client's
    /// global prior-knowledge setting.
    #[default]
    Auto,
    Http1,
    /// HTTP/2 over TLS: a connection that does not negotiate `h2` fails.
    H2,
    /// HTTP/2 with prior knowledge in cleartext.
    H2c,
}

impl HttpVersion {
    /// The configuration value: `auto`, `http1`, `h2` or `h2c`.
    #[must_use]
    pub fn parse(s: &str) -> Option<HttpVersion> {
        Some(match s {
            "auto" => HttpVersion::Auto,
            "http1" => HttpVersion::Http1,
            "h2" => HttpVersion::H2,
            "h2c" => HttpVersion::H2c,
            _ => return None,
        })
    }
}

/// The settings of one `tls` block, and the HTTP version of the destination.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TlsSettings {
    pub verify: bool,
    /// PEM bundle added to the default roots.
    pub ca_pem: Option<Vec<u8>>,
    /// SNI and verification name, instead of the URL host.
    pub server_name: Option<String>,
    pub min_tls13: bool,
    pub http: HttpVersion,
    /// The client certificate chain and key presented to the upstream (PEM).
    pub client_cert: Option<ClientCertPem>,
}

/// A client certificate chain and its private key, as PEM. The key is zeroed when dropped.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ClientCertPem {
    pub chain: Vec<u8>,
    pub key: antirion_core::Secret,
}

impl fmt::Debug for ClientCertPem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientCertPem").finish_non_exhaustive()
    }
}

impl Default for TlsSettings {
    fn default() -> TlsSettings {
        TlsSettings {
            verify: true,
            ca_pem: None,
            server_name: None,
            min_tls13: false,
            http: HttpVersion::Auto,
            client_cert: None,
        }
    }
}

/// Why a `tls` block cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum TlsConfigError {
    #[error("the CA bundle holds no usable certificate")]
    CaBundle,
    #[error("the server name is not a valid DNS name or IP address")]
    ServerName,
    #[error("the TLS settings cannot be built")]
    Build,
    #[error("the client certificate holds no certificate, or is not valid PEM")]
    ClientCert,
    #[error("the client key is not a valid PEM private key, or does not match the certificate")]
    ClientKey,
}

/// A compiled TLS profile. Connections are pooled per profile, so two profiles never share one.
#[derive(Clone)]
pub struct TlsProfile {
    pub(crate) key: u64,
    pub(crate) config: Arc<ClientConfig>,
    pub(crate) server_name: Option<ServerName<'static>>,
    pub verify: bool,
    pub http: HttpVersion,
}

impl fmt::Debug for TlsProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsProfile")
            .field("verify", &self.verify)
            .field("server_name", &self.server_name)
            .finish_non_exhaustive()
    }
}

/// The last step of a client config: the client certificate, when one is configured.
fn client_auth(
    builder: rustls::ConfigBuilder<ClientConfig, rustls::client::WantsClientCert>,
    cert: Option<&ClientCertPem>,
) -> Result<ClientConfig, TlsConfigError> {
    let Some(cert) = cert else {
        return Ok(builder.with_no_client_auth());
    };
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&cert.chain)
        .collect::<Result<_, _>>()
        .map_err(|_| TlsConfigError::ClientCert)?;
    if chain.is_empty() {
        return Err(TlsConfigError::ClientCert);
    }
    let key =
        PrivateKeyDer::from_pem_slice(cert.key.expose().as_bytes()).map_err(|_| TlsConfigError::ClientKey)?;
    builder
        .with_client_auth_cert(chain, key)
        .map_err(|_| TlsConfigError::ClientKey)
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

impl TlsProfile {
    pub fn build(s: &TlsSettings) -> Result<TlsProfile, TlsConfigError> {
        let provider = provider();
        let versions: &[&rustls::SupportedProtocolVersion] = if s.min_tls13 {
            &[&rustls::version::TLS13]
        } else {
            &[&rustls::version::TLS13, &rustls::version::TLS12]
        };
        let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_protocol_versions(versions)
            .map_err(|_| TlsConfigError::Build)?;
        let mut config = if s.verify {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            if let Some(pem) = &s.ca_pem {
                let mut added = 0usize;
                for cert in CertificateDer::pem_slice_iter(pem) {
                    let cert = cert.map_err(|_| TlsConfigError::CaBundle)?;
                    roots.add(cert).map_err(|_| TlsConfigError::CaBundle)?;
                    added += 1;
                }
                if added == 0 {
                    return Err(TlsConfigError::CaBundle);
                }
            }
            client_auth(builder.with_root_certificates(roots), s.client_cert.as_ref())?
        } else {
            client_auth(
                builder
                    .dangerous()
                    .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate(provider))),
                s.client_cert.as_ref(),
            )?
        };
        config.alpn_protocols = match s.http {
            HttpVersion::Http1 => vec![b"http/1.1".to_vec()],
            HttpVersion::H2 => vec![b"h2".to_vec()],
            HttpVersion::Auto | HttpVersion::H2c => vec![b"h2".to_vec(), b"http/1.1".to_vec()],
        };
        let server_name = match &s.server_name {
            None => None,
            Some(n) => Some(ServerName::try_from(n.clone()).map_err(|_| TlsConfigError::ServerName)?),
        };
        let mut h = std::hash::DefaultHasher::new();
        s.hash(&mut h);
        Ok(TlsProfile {
            key: h.finish(),
            config: Arc::new(config),
            server_name,
            verify: s.verify,
            http: s.http,
        })
    }

    /// The profile of an egress without a `tls` block of its own.
    pub fn default_profile() -> Result<TlsProfile, TlsConfigError> {
        TlsProfile::build(&TlsSettings::default())
    }

    /// The same profile offering only HTTP/1.1 through ALPN, for a connection that is upgraded
    /// (a WebSocket), which HTTP/2 cannot carry.
    #[must_use]
    pub fn http1_only(&self) -> TlsProfile {
        let mut config = (*self.config).clone();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        TlsProfile {
            key: self.key ^ 0x7769_7468_6831,
            config: Arc::new(config),
            server_name: self.server_name.clone(),
            verify: self.verify,
            http: HttpVersion::Http1,
        }
    }
}

/// `verify: false`: any certificate is accepted, but handshake signatures are still checked.
#[derive(Debug)]
struct AcceptAnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_differ_by_settings() {
        let a = TlsProfile::build(&TlsSettings::default()).unwrap();
        let b = TlsProfile::build(&TlsSettings {
            verify: false,
            ..TlsSettings::default()
        })
        .unwrap();
        assert_ne!(a.key, b.key);
        assert!(!b.verify);
        assert_eq!(
            a.config.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }

    #[test]
    fn the_http_version_selects_the_alpn_offer() {
        let alpn = |http| {
            TlsProfile::build(&TlsSettings {
                http,
                ..TlsSettings::default()
            })
            .unwrap()
            .config
            .alpn_protocols
            .clone()
        };
        assert_eq!(alpn(HttpVersion::Http1), vec![b"http/1.1".to_vec()]);
        assert_eq!(alpn(HttpVersion::H2), vec![b"h2".to_vec()]);
        assert_eq!(HttpVersion::parse("h2c"), Some(HttpVersion::H2c));
        assert_eq!(HttpVersion::parse("http2"), None);
    }

    #[test]
    fn a_bundle_without_certificates_is_refused() {
        let bad = TlsSettings {
            ca_pem: Some(b"not a certificate".to_vec()),
            ..TlsSettings::default()
        };
        assert!(matches!(TlsProfile::build(&bad), Err(TlsConfigError::CaBundle)));
        let name = TlsSettings {
            server_name: Some("bad name!".into()),
            ..TlsSettings::default()
        };
        assert!(matches!(
            TlsProfile::build(&name),
            Err(TlsConfigError::ServerName)
        ));
    }
}

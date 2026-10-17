//! TLS to PostgreSQL as the connection URL's `sslmode` and `sslrootcert` ask, with libpq's meaning:
//! `disable` never encrypts; `allow` and `prefer` (the default) encrypt when the server offers it,
//! without verifying; `require` encrypts without verifying, unless `sslrootcert` names a CA file,
//! when it verifies as `verify-ca` does; `verify-ca` checks the certificate chain against the
//! bundled web PKI roots plus `sslrootcert`; `verify-full` also checks the host name.
//! `sslrootcert=system` means the bundled roots alone.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_postgres::config::SslMode;
use tokio_postgres::tls::{ChannelBinding, MakeTlsConnect, TlsConnect};
use tokio_postgres::{Config, Socket};

use crate::StoreError;

/// How the server's certificate is checked.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Verify {
    Nothing,
    Chain,
    ChainAndName,
}

/// The TLS connector of a store's connections.
#[derive(Clone)]
pub(crate) struct PgTls {
    config: Arc<ClientConfig>,
}

impl std::fmt::Debug for PgTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgTls").finish_non_exhaustive()
    }
}

fn invalid(what: &str) -> StoreError {
    StoreError::Config(format!("store.postgres.url: {what}"))
}

/// Decodes `%XX` escapes of a URL query value.
fn percent_decode(v: &str) -> Result<String, StoreError> {
    let bytes = v.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| invalid("a query value has a malformed escape"))?;
            out.push(hex);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| invalid("a query value is not UTF-8"))
}

/// Takes `sslmode` and `sslrootcert` out of a URL or a key=value connection string, which the
/// driver would refuse or ignore.
fn take_tls_params(url: &str) -> Result<(String, Option<String>, Option<String>), StoreError> {
    let mut mode = None;
    let mut root = None;
    if url.contains("://") {
        let Some((base, query)) = url.split_once('?') else {
            return Ok((url.to_owned(), None, None));
        };
        let mut kept = Vec::new();
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("sslmode", v)) => mode = Some(percent_decode(v)?),
                Some(("sslrootcert", v)) => root = Some(percent_decode(v)?),
                _ => kept.push(pair),
            }
        }
        let rest = if kept.is_empty() {
            base.to_owned()
        } else {
            format!("{base}?{}", kept.join("&"))
        };
        return Ok((rest, mode, root));
    }
    let mut kept = Vec::new();
    for token in url.split_whitespace() {
        match token.split_once('=') {
            Some(("sslmode", v)) => mode = Some(v.trim_matches('\'').to_owned()),
            Some(("sslrootcert", v)) => root = Some(v.trim_matches('\'').to_owned()),
            _ => kept.push(token),
        }
    }
    Ok((kept.join(" "), mode, root))
}

/// The driver's configuration and the TLS connector of a connection URL.
pub(crate) fn parse(url: &str) -> Result<(Config, PgTls), StoreError> {
    let (rest, mode, root) = take_tls_params(url)?;
    let mut config: Config = rest.parse().map_err(|_| invalid("not a valid PostgreSQL URL"))?;
    let (ssl_mode, mut verify) = match mode.as_deref().unwrap_or("prefer") {
        "disable" => (SslMode::Disable, Verify::Nothing),
        "allow" | "prefer" => (SslMode::Prefer, Verify::Nothing),
        "require" => (SslMode::Require, Verify::Nothing),
        "verify-ca" => (SslMode::Require, Verify::Chain),
        "verify-full" => (SslMode::Require, Verify::ChainAndName),
        _ => {
            return Err(invalid(
                "sslmode must be disable, allow, prefer, require, verify-ca or verify-full",
            ));
        }
    };
    let ca = match root.as_deref() {
        None | Some("system") => None,
        Some(path) => Some(std::fs::read(path).map_err(|_| invalid("sslrootcert cannot be read"))?),
    };
    if ca.is_some() && verify == Verify::Nothing && ssl_mode == SslMode::Require {
        verify = Verify::Chain;
    }
    config.ssl_mode(ssl_mode);
    Ok((config, PgTls::build(verify, ca.as_deref())?))
}

impl PgTls {
    fn build(verify: Verify, ca_pem: Option<&[u8]>) -> Result<PgTls, StoreError> {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
            .map_err(|_| invalid("TLS cannot be set up"))?;
        let config = if verify == Verify::Nothing {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
                .with_no_client_auth()
        } else {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            if let Some(pem) = ca_pem {
                let mut added = 0usize;
                for cert in CertificateDer::pem_slice_iter(pem) {
                    let cert = cert.map_err(|_| invalid("sslrootcert is not a PEM bundle"))?;
                    roots
                        .add(cert)
                        .map_err(|_| invalid("sslrootcert holds an unusable certificate"))?;
                    added += 1;
                }
                if added == 0 {
                    return Err(invalid("sslrootcert holds no certificate"));
                }
            }
            let inner = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
                .build()
                .map_err(|_| invalid("TLS cannot be set up"))?;
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(Verified {
                    inner,
                    check_name: verify == Verify::ChainAndName,
                }))
                .with_no_client_auth()
        };
        Ok(PgTls {
            config: Arc::new(config),
        })
    }
}

impl MakeTlsConnect<Socket> for PgTls {
    type Stream = PgTlsStream;
    type TlsConnect = Connect;
    type Error = io::Error;

    fn make_tls_connect(&mut self, domain: &str) -> Result<Connect, io::Error> {
        Ok(Connect {
            connector: tokio_rustls::TlsConnector::from(Arc::clone(&self.config)),
            name: ServerName::try_from(domain.to_owned()).ok(),
        })
    }
}

/// One TLS handshake; a host name that is not a valid server name fails only when TLS is used.
pub(crate) struct Connect {
    connector: tokio_rustls::TlsConnector,
    name: Option<ServerName<'static>>,
}

impl TlsConnect<Socket> for Connect {
    type Stream = PgTlsStream;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<PgTlsStream>> + Send>>;

    fn connect(self, stream: Socket) -> Self::Future {
        Box::pin(async move {
            let name = self.name.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the store host is not a valid TLS server name",
                )
            })?;
            let tls = self.connector.connect(name, stream).await?;
            Ok(PgTlsStream(Box::new(tls)))
        })
    }
}

/// An encrypted store connection.
pub(crate) struct PgTlsStream(Box<tokio_rustls::client::TlsStream<Socket>>);

impl AsyncRead for PgTlsStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for PgTlsStream {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_shutdown(cx)
    }
}

impl tokio_postgres::tls::TlsStream for PgTlsStream {
    fn channel_binding(&self) -> ChannelBinding {
        ChannelBinding::none()
    }
}

/// `verify-ca` and `verify-full`: the chain is checked; the name only for `verify-full`.
#[derive(Debug)]
struct Verified {
    inner: Arc<WebPkiServerVerifier>,
    check_name: bool,
}

impl ServerCertVerifier for Verified {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self
            .inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
        {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) if !self.check_name => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// `prefer` and `require` without a CA: any certificate, but handshake signatures are checked.
#[derive(Debug)]
struct AnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
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
    fn tls_parameters_leave_the_url() {
        let (rest, mode, root) = take_tls_params(
            "postgres://u:p@db:5432/x?sslmode=verify-full&application_name=a&sslrootcert=%2Fca.pem",
        )
        .unwrap();
        assert_eq!(rest, "postgres://u:p@db:5432/x?application_name=a");
        assert_eq!(mode.as_deref(), Some("verify-full"));
        assert_eq!(root.as_deref(), Some("/ca.pem"));
        let (rest, mode, root) = take_tls_params("host=db sslmode=require user=u").unwrap();
        assert_eq!(rest, "host=db user=u");
        assert_eq!(mode.as_deref(), Some("require"));
        assert_eq!(root, None);
        assert_eq!(
            take_tls_params("postgres://db/x").unwrap(),
            ("postgres://db/x".to_owned(), None, None)
        );
    }

    #[test]
    fn modes_map_to_the_driver() {
        for (mode, want) in [
            ("disable", SslMode::Disable),
            ("prefer", SslMode::Prefer),
            ("require", SslMode::Require),
            ("verify-full", SslMode::Require),
        ] {
            let (config, _) = parse(&format!("postgres://db/x?sslmode={mode}")).unwrap();
            assert_eq!(config.get_ssl_mode(), want, "{mode}");
        }
        let (config, _) = parse("postgres://db/x").unwrap();
        assert_eq!(config.get_ssl_mode(), SslMode::Prefer);
        assert!(parse("postgres://db/x?sslmode=sometimes").is_err());
        assert!(parse("postgres://db/x?sslmode=verify-ca&sslrootcert=/no/such/file").is_err());
    }
}

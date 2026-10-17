//! Listener TLS: TLS 1.3 and (unless the minimum is 1.3) TLS 1.2, with the ECDHE AEAD suites of the
//! crypto provider only; certificate and key files re-read every reload interval, a valid new pair
//! used for new handshakes, and an invalid one ignored while the previous pair keeps serving. With
//! `client_auth`, client certificates are verified against `client_ca_file` (re-read the same way):
//! required, or verified only when presented.

use std::sync::Arc;
use std::time::Duration;

use antirion_runtime::Runtime;
use antirion_runtime::security::{ClientAuth, ListenerTls, SecurityRt};
use arc_swap::ArcSwap;
use rustls::server::WebPkiClientVerifier;
use rustls::sign::CertifiedKey;
use rustls::{RootCertStore, ServerConfig};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

/// Which listener a TLS setting belongs to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Plane {
    Data,
    Admin,
}

impl Plane {
    fn settings(self, s: &SecurityRt) -> Option<&ListenerTls> {
        match self {
            Plane::Data => s.tls_data.as_ref(),
            Plane::Admin => s.tls_admin.as_ref(),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Plane::Data => "data",
            Plane::Admin => "admin",
        }
    }
}

/// Why a certificate pair cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum TlsLoadError {
    #[error("cannot read {0}")]
    Read(String),
    #[error("the certificate file holds no certificate")]
    NoCertificate,
    #[error("the certificate or key file is not valid PEM")]
    Pem,
    #[error("the key does not match the certificate, or is not supported")]
    Key,
    #[error("the TLS settings cannot be built")]
    Build,
    #[error("the client CA file holds no usable certificate")]
    ClientCa,
}

/// The files and settings a config was built from, to notice changes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Loaded {
    settings: ListenerTls,
    cert: Vec<u8>,
    key: Vec<u8>,
    client_ca: Option<Vec<u8>>,
}

async fn read(settings: &ListenerTls) -> Result<Loaded, TlsLoadError> {
    let cert = tokio::fs::read(&settings.cert_file)
        .await
        .map_err(|_| TlsLoadError::Read(settings.cert_file.display().to_string()))?;
    let key = tokio::fs::read(&settings.key_file)
        .await
        .map_err(|_| TlsLoadError::Read(settings.key_file.display().to_string()))?;
    let client_ca = match (&settings.client_ca_file, settings.client_auth) {
        (Some(path), ClientAuth::Optional | ClientAuth::Required) => Some(
            tokio::fs::read(path)
                .await
                .map_err(|_| TlsLoadError::Read(path.display().to_string()))?,
        ),
        _ => None,
    };
    Ok(Loaded {
        settings: settings.clone(),
        cert,
        key,
        client_ca,
    })
}

/// The verifier of client certificates, for a listener with `client_auth`.
fn client_verifier(
    l: &Loaded,
    provider: &Arc<rustls::crypto::CryptoProvider>,
) -> Result<Option<Arc<dyn rustls::server::danger::ClientCertVerifier>>, TlsLoadError> {
    let Some(pem) = l.client_ca.as_deref() else {
        return Ok(None);
    };
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(pem) {
        roots
            .add(cert.map_err(|_| TlsLoadError::ClientCa)?)
            .map_err(|_| TlsLoadError::ClientCa)?;
    }
    if roots.is_empty() {
        return Err(TlsLoadError::ClientCa);
    }
    let builder = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), Arc::clone(provider));
    let builder = match l.settings.client_auth {
        ClientAuth::Optional => builder.allow_unauthenticated(),
        _ => builder,
    };
    builder.build().map(Some).map_err(|_| TlsLoadError::ClientCa)
}

/// The client certificate a connection's handshake verified: its digest and its names.
pub(crate) fn client_certificate(
    conn: &rustls::ServerConnection,
) -> Option<std::sync::Arc<antirion_core::ClientCert>> {
    let leaf = conn.peer_certificates()?.first()?;
    Some(std::sync::Arc::new(antirion_identity::certificate::client_cert(
        leaf.as_ref(),
    )))
}

fn build(l: &Loaded) -> Result<Arc<ServerConfig>, TlsLoadError> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&l.cert)
        .collect::<Result<_, _>>()
        .map_err(|_| TlsLoadError::Pem)?;
    if chain.is_empty() {
        return Err(TlsLoadError::NoCertificate);
    }
    let key = PrivateKeyDer::from_pem_slice(&l.key).map_err(|_| TlsLoadError::Pem)?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let certified = CertifiedKey::from_der(chain, key, &provider).map_err(|_| TlsLoadError::Key)?;
    let versions: &[&rustls::SupportedProtocolVersion] = if l.settings.min_tls13 {
        &[&rustls::version::TLS13]
    } else {
        &[&rustls::version::TLS13, &rustls::version::TLS12]
    };
    let verifier = client_verifier(l, &provider)?;
    let builder = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .map_err(|_| TlsLoadError::Build)?;
    let builder = match verifier {
        Some(v) => builder.with_client_cert_verifier(v),
        None => builder.with_no_client_auth(),
    };
    let mut config = builder.with_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(certified)));
    config.alpn_protocols.clone_from(&l.settings.alpn);
    Ok(Arc::new(config))
}

/// The acceptor of one listener, swapped when its files change.
pub(crate) struct Reloading {
    plane: Plane,
    current: ArcSwap<TlsAcceptor>,
    loaded: parking_lot::Mutex<Loaded>,
    /// The contents of the last pair that failed, so a broken file is reported once.
    rejected: parking_lot::Mutex<Option<Loaded>>,
}

impl std::fmt::Debug for Reloading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reloading")
            .field("plane", &self.plane)
            .finish_non_exhaustive()
    }
}

impl Reloading {
    /// The acceptor for a listener the snapshot configures with TLS, or `None` for plaintext.
    pub(crate) async fn start(
        security: &SecurityRt,
        plane: Plane,
    ) -> Result<Option<Arc<Reloading>>, TlsLoadError> {
        let Some(settings) = plane.settings(security) else {
            return Ok(None);
        };
        let loaded = read(settings).await?;
        let config = build(&loaded)?;
        Ok(Some(Arc::new(Reloading {
            plane,
            current: ArcSwap::from_pointee(TlsAcceptor::from(config)),
            loaded: parking_lot::Mutex::new(loaded),
            rejected: parking_lot::Mutex::new(None),
        })))
    }

    pub(crate) fn acceptor(&self) -> Arc<TlsAcceptor> {
        self.current.load_full()
    }

    async fn reload(&self, settings: &ListenerTls) {
        let loaded = match read(settings).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(listener = self.plane.name(), error = %e, "cannot reload the TLS certificate; the previous one keeps serving");
                return;
            }
        };
        if *self.loaded.lock() == loaded || self.rejected.lock().as_ref() == Some(&loaded) {
            return;
        }
        match build(&loaded) {
            Ok(config) => {
                self.current.store(Arc::new(TlsAcceptor::from(config)));
                *self.loaded.lock() = loaded;
                *self.rejected.lock() = None;
                tracing::info!(listener = self.plane.name(), "reloaded the TLS certificate");
            }
            Err(e) => {
                *self.rejected.lock() = Some(loaded);
                tracing::error!(listener = self.plane.name(), error = %e, "the new TLS certificate is invalid; the previous one keeps serving");
            }
        }
    }

    /// Re-reads the files every reload interval until `stop`.
    pub(crate) async fn watch(self: Arc<Self>, rt: Arc<Runtime>, stop: CancellationToken) {
        loop {
            let snap = rt.gens.load();
            let interval = snap.security.tls_reload.max(Duration::from_millis(100));
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(interval) => {}
            }
            let snap = rt.gens.load();
            if let Some(settings) = self.plane.settings(&snap.security) {
                self.reload(settings).await;
            }
        }
    }
}

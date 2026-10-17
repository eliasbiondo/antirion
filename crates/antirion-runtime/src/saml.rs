//! What SAML sign-ins need of the node: the identity provider of each provider (its metadata,
//! fetched from `metadata_url` as operator egress and kept in the store so every node uses the same
//! copy; a failed fetch keeps the stored copy), and the assertion ids already accepted, kept in the
//! store until the assertion could no longer be accepted, so an assertion is used once across nodes.

use std::sync::Arc;
use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_egress::{OutboundRequest, TrustClass};
use antirion_identity::saml::{IdpDescriptor, IdpSource, Provider, is_plaintext, parse_idp_metadata};
use antirion_store::{IdentityRecord, StoreError};
use bytes::BytesMut;
use futures_util::StreamExt;
use http::{HeaderMap, HeaderValue, Method, Uri};
use http_body_util::BodyExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Runtime;

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_METADATA: usize = 4 * 1024 * 1024;
const ASSERTION_KIND: &str = "saml_assertion";

/// The stored copy of a provider's metadata.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredMetadata {
    pub url: String,
    /// When `xml` was fetched (gateway clock), 0 before the first success.
    pub fetched_us: i64,
    pub xml: String,
    /// When a fetch was last attempted, and why it failed if it did.
    pub attempted_us: i64,
    pub last_error: Option<String>,
}

/// Why no identity provider is available.
#[derive(Debug, thiserror::Error)]
pub enum IdpUnavailable {
    #[error("the identity provider's metadata could not be fetched")]
    Fetch,
    #[error("the identity provider's metadata cannot be used: {0}")]
    Unusable(String),
    #[error("the store cannot be read")]
    Store(#[from] StoreError),
}

/// Metadata parsed from a stored copy: the copy's URL and fetch time, and what it says.
#[derive(Debug)]
struct Parsed {
    url: String,
    fetched_us: i64,
    idp: Arc<IdpDescriptor>,
}

/// Parsed metadata by provider, valid while the stored copy is the one it was parsed from.
#[derive(Debug, Default)]
pub struct SamlState {
    parsed: Mutex<std::collections::HashMap<String, Parsed>>,
}

fn key(provider: &str) -> String {
    format!("saml_metadata/{provider}")
}

impl Runtime {
    /// The stored metadata of `provider`, when there is one.
    pub async fn saml_stored(&self, provider: &str) -> Result<Option<StoredMetadata>, StoreError> {
        let raw = self.store.value(&key(provider)).await?;
        Ok(raw.and_then(|r| serde_json::from_slice(&r).ok()))
    }

    async fn fetch_metadata(&self, url: &str, insecure: bool) -> Result<String, String> {
        let snap = self.gens.load();
        let tls = snap
            .security
            .egress_tls
            .clone()
            .ok_or_else(|| "no TLS profile for egress".to_owned())?;
        if !insecure && self.test.is_none() {
            if is_plaintext(url) {
                return Err("the metadata URL must use https".to_owned());
            }
            if !tls.verify {
                return Err(
                    "the egress TLS profile does not verify servers, so the metadata cannot be trusted"
                        .to_owned(),
                );
            }
        }
        let uri: Uri = url.parse().map_err(|_| "the URL is not valid".to_owned())?;
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::ACCEPT,
            HeaderValue::from_static("application/samlmetadata+xml, application/xml, text/xml"),
        );
        headers.insert(http::header::USER_AGENT, HeaderValue::from_static("antirion"));
        let request = OutboundRequest {
            method: Method::GET,
            uri,
            headers,
            body: bytes::Bytes::new().into(),
            response_timeout: FETCH_TIMEOUT,
            connect_timeout: None,
            fresh_connection: false,
            trust: TrustClass::Operator,
            tls,
            policy: Arc::clone(&snap.security.egress),
            resend_when_closed: false,
        };
        let read = async {
            let resp = self
                .egress
                .send(request)
                .await
                .map_err(|_| "the metadata URL could not be reached".to_owned())?;
            if !resp.status.is_success() {
                return Err(format!(
                    "the metadata URL answered with status {}",
                    resp.status.as_u16()
                ));
            }
            let mut body = resp.body.into_data_stream();
            let mut out = BytesMut::new();
            while let Some(chunk) = body.next().await {
                let chunk = chunk.map_err(|_| "the metadata answer was cut short".to_owned())?;
                if out.len() + chunk.len() > MAX_METADATA {
                    return Err("the metadata is too large".to_owned());
                }
                out.extend_from_slice(&chunk);
            }
            String::from_utf8(out.to_vec()).map_err(|_| "the metadata is not UTF-8".to_owned())
        };
        tokio::time::timeout(FETCH_TIMEOUT, read)
            .await
            .map_err(|_| "the metadata URL did not answer in time".to_owned())?
    }

    /// Fetches `provider`'s metadata from `url` now and stores it when it can be used; the stored
    /// copy records the attempt either way.
    pub async fn saml_resync(
        &self,
        provider: &str,
        url: &str,
        insecure: bool,
    ) -> Result<StoredMetadata, StoreError> {
        let now = self.now().0;
        let mut stored = self
            .saml_stored(provider)
            .await?
            .filter(|s| s.url == url)
            .unwrap_or_else(|| StoredMetadata {
                url: url.to_owned(),
                ..StoredMetadata::default()
            });
        stored.attempted_us = now;
        match self.fetch_metadata(url, insecure).await {
            Ok(xml) => match parse_idp_metadata(&xml) {
                Ok(_) => {
                    stored.xml = xml;
                    stored.fetched_us = now;
                    stored.last_error = None;
                }
                Err(e) => stored.last_error = Some(e.to_string()),
            },
            Err(e) => stored.last_error = Some(e),
        }
        if stored.last_error.is_some() {
            tracing::warn!(
                provider,
                "the SAML identity provider's metadata could not be refreshed"
            );
        }
        self.store
            .put_value(&key(provider), serde_json::to_vec(&stored).unwrap_or_default())
            .await?;
        Ok(stored)
    }

    /// The identity provider of `provider`: as configured, or its metadata as stored, fetched first
    /// when there is no copy for its URL or the copy is older than the refresh interval. A stale copy
    /// is used when a refresh fails.
    pub async fn saml_idp(
        &self,
        name: &str,
        provider: &Provider,
    ) -> Result<Arc<IdpDescriptor>, IdpUnavailable> {
        let (url, refresh, insecure) = match &provider.idp {
            IdpSource::Fixed(d) => return Ok(Arc::new(d.clone())),
            IdpSource::MetadataUrl {
                url,
                refresh,
                insecure,
            } => (url, *refresh, *insecure),
        };
        let now = self.now();
        let mut stored = self.saml_stored(name).await?.filter(|s| s.url == *url);
        let fresh = stored
            .as_ref()
            .is_some_and(|s| s.fetched_us > 0 && now.since(UtcMicros(s.fetched_us)) < refresh);
        let retried_lately = stored
            .as_ref()
            .is_some_and(|s| now.since(UtcMicros(s.attempted_us)) < Duration::from_secs(60));
        if !fresh && !(retried_lately && stored.as_ref().is_some_and(|s| s.fetched_us > 0)) {
            stored = Some(self.saml_resync(name, url, insecure).await?);
        }
        let stored = stored.filter(|s| s.fetched_us > 0).ok_or(IdpUnavailable::Fetch)?;
        if let Some(p) = self.state.saml.parsed.lock().get(name)
            && p.url == stored.url
            && p.fetched_us == stored.fetched_us
        {
            return Ok(Arc::clone(&p.idp));
        }
        let parsed =
            Arc::new(parse_idp_metadata(&stored.xml).map_err(|e| IdpUnavailable::Unusable(e.to_string()))?);
        self.state.saml.parsed.lock().insert(
            name.to_owned(),
            Parsed {
                url: stored.url.clone(),
                fetched_us: stored.fetched_us,
                idp: Arc::clone(&parsed),
            },
        );
        Ok(parsed)
    }

    /// Records that `provider`'s assertion `id` was accepted, until `valid_until_us`; false when it
    /// was accepted before.
    pub async fn saml_assertion_once(
        &self,
        provider: &str,
        id: &str,
        valid_until_us: i64,
    ) -> Result<bool, StoreError> {
        let digest = Sha256::digest(format!("{provider}\n{id}").as_bytes());
        let id = digest.iter().fold(String::with_capacity(64), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        });
        self.store
            .put_record_once(
                IdentityRecord {
                    kind: ASSERTION_KIND.to_owned(),
                    id,
                    user: String::new(),
                    expires_us: Some(valid_until_us),
                    body: "{}".to_owned(),
                },
                self.now().0,
            )
            .await
    }
}

//! Calls to MCP servers and agents go through the gateway's one egress path: operator trust (only
//! gateway-scope principals register servers and agents), the egress policy and the TLS profile of
//! `security.egress`.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use antirion_core::RequestId;
use antirion_core::headers::X_REQUEST_ID;
use antirion_egress::ssrf::{self, Denied};
use antirion_egress::{
    EgressClient, EgressPolicy, InboundResponse, OutboundRequest, TlsProfile, TransportError, TrustClass,
};
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Uri};

/// The egress client with the policy and TLS profile of the current generation.
#[derive(Clone, Debug)]
pub struct Egress {
    pub client: EgressClient,
    pub policy: Arc<EgressPolicy>,
    pub tls: TlsProfile,
    /// The id of the request the calls are made for, sent as `x-request-id`.
    pub request_id: RequestId,
    /// The JWT the caller presented, for servers that exchange it for their own token.
    pub subject: Option<Arc<str>>,
}

impl Egress {
    /// Sends one request to an operator-configured destination: one attempt, never re-sent, since
    /// the server's retry rule decides what may be sent again.
    pub(crate) async fn send(
        &self,
        method: Method,
        uri: Uri,
        mut headers: HeaderMap,
        body: Bytes,
        response_timeout: Duration,
    ) -> Result<InboundResponse, TransportError> {
        if let Ok(v) = HeaderValue::from_str(self.request_id.as_str()) {
            headers.insert(HeaderName::from_static(X_REQUEST_ID), v);
        }
        self.client
            .send(OutboundRequest {
                method,
                uri,
                headers,
                body: body.into(),
                response_timeout,
                connect_timeout: None,
                fresh_connection: false,
                trust: TrustClass::Operator,
                tls: self.tls.clone(),
                policy: Arc::clone(&self.policy),
                resend_when_closed: false,
            })
            .await
    }

    /// Why a URL taken from request content would be refused for egress, judged by the URL rules and
    /// the address rules: on an IP literal, else on every address its host resolves to here (a
    /// name that does not resolve here is judged by the URL rules only, since another party
    /// calls it).
    pub(crate) async fn refuse_client_url(&self, url: &str) -> Option<Denied> {
        let v = match ssrf::validate_url(url, TrustClass::ClientInfluenced, &self.policy) {
            Ok(v) => v,
            Err(d) => return Some(d),
        };
        let check =
            |ip: IpAddr| ssrf::check_addr(ip, &v.host, TrustClass::ClientInfluenced, &self.policy).err();
        if let Some(ip) = v.ip.or_else(|| v.host.parse::<IpAddr>().ok()) {
            return check(ip);
        }
        let resolved = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::lookup_host((v.host.as_str(), v.port)),
        )
        .await;
        match resolved {
            Ok(Ok(addrs)) => addrs.map(|a| a.ip()).find_map(check),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn egress() -> Egress {
        Egress {
            client: EgressClient::new(Duration::from_secs(1)),
            policy: Arc::new(EgressPolicy::default()),
            tls: TlsProfile::default_profile().unwrap(),
            request_id: RequestId::generate(),
            subject: None,
        }
    }

    #[tokio::test]
    async fn client_url_rules() {
        let e = egress();
        for bad in [
            "http://169.254.169.254/latest/meta-data",
            "https://127.0.0.1/hook",
            "https://10.0.0.7/hook",
            "http://93.184.215.14/hook",
            "https://metadata.google.internal/x",
            "https://user:p@hooks.example.com/",
            "https://hooks.example.com:8443/",
            "https://[::1]/x",
        ] {
            assert!(e.refuse_client_url(bad).await.is_some(), "{bad}");
        }
        for good in ["https://93.184.215.14/hook", "https://hooks.example.com/a?b=c"] {
            assert!(e.refuse_client_url(good).await.is_none(), "{good}");
        }
    }
}

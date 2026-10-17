//! Requests forwarded under an explicit passthrough mount: the client's method, path below the
//! mount, query and body, sent to the provider's base URL with the provider's own credential.

use std::sync::Arc;
use std::time::Duration;

use antirion_egress::{OutboundRequest, Payload, TrustClass};
use http::{HeaderMap, Method, Uri};

use crate::{BuildError, DeploymentRt, UpstreamAuth};

/// What a passthrough request forwards.
#[derive(Debug)]
pub struct Forward<'a> {
    pub method: Method,
    /// The path below the mount's prefix (empty or starting with `/`).
    pub path: &'a str,
    /// The client's query string, unchanged.
    pub query: Option<&'a str>,
    pub headers: HeaderMap,
    pub body: Payload,
    pub connect_timeout: Option<Duration>,
}

/// `base` joined with `path`, a leading `/v1` of `path` not repeated when the base already ends in
/// `/v1`, then the base's query and the client's.
fn url(base: &str, path: &str, query: Option<&str>) -> String {
    let (base, base_query) = base.split_once('?').unwrap_or((base, ""));
    let base = base.trim_end_matches('/');
    let path = match path.strip_prefix("/v1") {
        Some(rest) if base.ends_with("/v1") && (rest.is_empty() || rest.starts_with('/')) => rest,
        _ => path,
    };
    let mut out = format!("{base}{path}");
    let queries: Vec<&str> = [base_query, query.unwrap_or_default()]
        .into_iter()
        .filter(|q| !q.is_empty())
        .collect();
    if !queries.is_empty() {
        out.push('?');
        out.push_str(&queries.join("&"));
    }
    out
}

impl DeploymentRt {
    /// The upstream request of a passthrough mount on this deployment's provider: its static key
    /// in the type's key header when it has one; [`DeploymentRt::authorize`] adds a token or a
    /// signature.
    pub fn passthrough_request(&self, f: Forward<'_>) -> Result<OutboundRequest, BuildError> {
        let p = &self.provider;
        if let UpstreamAuth::Unsupported(_) = p.auth {
            return Err(BuildError::Unsupported);
        }
        let uri: Uri = url(&self.base()?, f.path, f.query)
            .parse()
            .map_err(|_| BuildError::InvalidUrl)?;
        let mut headers = f.headers;
        for (name, value) in p.headers.iter() {
            headers.insert(name.clone(), value.clone());
        }
        if let UpstreamAuth::Header { name, value } = &p.auth {
            headers.insert(name.clone(), value.clone());
        }
        Ok(OutboundRequest {
            method: f.method,
            uri,
            headers,
            body: f.body,
            response_timeout: Duration::from_secs(86_400 * 365),
            connect_timeout: f.connect_timeout,
            fresh_connection: false,
            trust: TrustClass::Operator,
            tls: p.tls.clone(),
            policy: Arc::clone(&p.egress),
            resend_when_closed: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::url;

    #[test]
    fn a_leading_v1_is_not_repeated() {
        assert_eq!(
            url("http://h/up/v1", "/v1/fake_endpoint", Some("x=1")),
            "http://h/up/v1/fake_endpoint?x=1"
        );
        assert_eq!(url("http://h/up/v1/", "/v1", None), "http://h/up/v1");
        assert_eq!(url("http://h/up", "/v1/files", None), "http://h/up/v1/files");
        assert_eq!(
            url("http://h/up/v1", "/v1beta/x", None),
            "http://h/up/v1/v1beta/x"
        );
        assert_eq!(url("http://h/b?k=1", "/x", Some("y=2")), "http://h/b/x?k=1&y=2");
    }
}

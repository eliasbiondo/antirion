//! Calls to a provider's other services (guardrail services of the same cloud), authorised with
//! the provider's own credentials.

use antirion_egress::{OutboundRequest, TlsProfile};
use http::HeaderValue;
use http::header::AUTHORIZATION;

use crate::creds::{CredError, Credentials};
use crate::request::sign_bedrock;
use crate::{ProviderRt, UpstreamAuth};

impl ProviderRt {
    /// Adds the provider's credential to `req`: a SigV4 signature (service `bedrock`) in `region`
    /// (else the provider's region), an access token, or its configured key header.
    pub async fn authorize_service(
        &self,
        req: &mut OutboundRequest,
        creds: &Credentials,
        cred_tls: &TlsProfile,
        region: Option<&str>,
    ) -> Result<(), CredError> {
        let region = region.or(self.region.as_deref()).unwrap_or("us-east-1");
        let policy = std::sync::Arc::clone(&req.policy);
        match &self.auth {
            UpstreamAuth::Aws(configured) => {
                let c = creds.aws(configured.as_ref(), region, cred_tls, &policy).await?;
                sign_bedrock(req, &c, region);
            }
            UpstreamAuth::AwsAssumed { base, role } => {
                let c = creds
                    .aws_assumed(base.as_ref(), role, region, cred_tls, &policy)
                    .await?;
                sign_bedrock(req, &c, region);
            }
            UpstreamAuth::Header { name, value } => {
                req.headers.insert(name.clone(), value.clone());
            }
            auth => {
                if let Some(source) = auth.token_source() {
                    let token = creds.token(&source, cred_tls, &policy).await?;
                    let mut v = HeaderValue::from_str(&format!("Bearer {token}"))
                        .map_err(|_| CredError::Malformed)?;
                    v.set_sensitive(true);
                    req.headers.insert(AUTHORIZATION, v);
                }
            }
        }
        Ok(())
    }
}

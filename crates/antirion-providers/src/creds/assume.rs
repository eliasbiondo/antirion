//! STS `AssumeRole`: temporary credentials for a role, requested with the provider's base
//! credentials (static or from the default chain) and signed with SigV4 for service `sts`.

use antirion_egress::TlsProfile;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue, Method, Uri};

use super::CredError;
use super::aws::{Fetched, expiration, xml_tag};
use super::http::{CredClient, form, send};
use crate::sigv4::{self, AwsCredentials};

/// The `auth.assume_role` block of a Bedrock provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssumeRole {
    pub role_arn: String,
    pub external_id: Option<String>,
    pub session_name: String,
    pub duration_secs: u64,
    /// The STS endpoint; the regional AWS endpoint when unset.
    pub sts_endpoint: Option<String>,
}

impl AssumeRole {
    /// The cache key of the role assumed with the base credentials `base_key_id`.
    pub(crate) fn cache_key(&self, base_key_id: &str, region: &str) -> String {
        format!(
            "{}|{}|{}|{}|{}|{base_key_id}|{region}",
            self.role_arn,
            self.external_id.as_deref().unwrap_or_default(),
            self.session_name,
            self.duration_secs,
            self.sts_endpoint.as_deref().unwrap_or_default(),
        )
    }

    /// The STS URL: the operation's request URI `/` below the endpoint's path, as the AWS SDKs join it.
    fn endpoint(&self, region: &str) -> String {
        match &self.sts_endpoint {
            Some(e) if e.ends_with('/') || e.contains('?') => e.clone(),
            Some(e) => format!("{e}/"),
            None => format!("https://sts.{region}.amazonaws.com/"),
        }
    }
}

fn amz_date(now: i64) -> String {
    jiff::Timestamp::from_second(now)
        .map(|t| t.strftime("%Y%m%dT%H%M%SZ").to_string())
        .unwrap_or_default()
}

/// Assumes `role` with `base`; `now` is Unix seconds on the real clock (the signing time).
pub(crate) async fn assume(
    client: &CredClient,
    tls: &TlsProfile,
    role: &AssumeRole,
    base: &AwsCredentials,
    region: &str,
    now: i64,
) -> Result<Fetched, CredError> {
    let url = role.endpoint(region);
    let uri: Uri = url.parse().map_err(|_| CredError::Endpoint)?;
    let duration = role.duration_secs.to_string();
    let mut pairs = vec![
        ("Action", "AssumeRole"),
        ("Version", "2011-06-15"),
        ("RoleArn", role.role_arn.as_str()),
        ("RoleSessionName", role.session_name.as_str()),
        ("DurationSeconds", duration.as_str()),
    ];
    if let Some(id) = &role.external_id {
        pairs.push(("ExternalId", id.as_str()));
    }
    let body = form(&pairs);
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded; charset=utf-8"),
    );
    sigv4::sign(
        &mut headers,
        &sigv4::SigningInput {
            method: &Method::POST,
            uri: &uri,
            body: &body,
            region,
            service: "sts",
            amz_date: &amz_date(now),
        },
        base,
    );
    let reply = send(client, tls, Method::POST, &url, headers, body).await?;
    if !(200..300).contains(&reply.status) {
        return Err(CredError::Rejected(reply.status));
    }
    let xml = String::from_utf8_lossy(&reply.body);
    Ok(Fetched {
        creds: AwsCredentials {
            access_key_id: xml_tag(&xml, "AccessKeyId").ok_or(CredError::Malformed)?,
            secret_access_key: xml_tag(&xml, "SecretAccessKey")
                .map(antirion_core::Secret::new)
                .ok_or(CredError::Malformed)?,
            session_token: Some(
                xml_tag(&xml, "SessionToken")
                    .map(antirion_core::Secret::new)
                    .ok_or(CredError::Malformed)?,
            ),
        },
        expires: expiration(xml_tag(&xml, "Expiration").as_deref()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_regional_endpoint_is_the_default() {
        let role = AssumeRole {
            role_arn: "arn:aws:iam::1:role/r".into(),
            external_id: None,
            session_name: "antirion".into(),
            duration_secs: 3600,
            sts_endpoint: None,
        };
        assert_eq!(role.endpoint("eu-west-1"), "https://sts.eu-west-1.amazonaws.com/");
        let private = AssumeRole {
            sts_endpoint: Some("https://sts.internal/aws".into()),
            ..role.clone()
        };
        assert_eq!(private.endpoint("x"), "https://sts.internal/aws/");
        let other = AssumeRole {
            external_id: Some("x".into()),
            ..role.clone()
        };
        assert_ne!(role.cache_key("AK", "r"), other.cache_key("AK", "r"));
        assert_eq!(amz_date(0), "19700101T000000Z");
    }
}

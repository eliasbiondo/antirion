//! The AWS default credential chain: environment, shared credentials file, web identity, container
//! credentials, then EC2 instance metadata (IMDSv2), in the order of the AWS SDKs.

use std::net::IpAddr;

use antirion_core::UtcMicros;
use antirion_egress::TlsProfile;
use bytes::Bytes;
use http::header::AUTHORIZATION;
use http::{HeaderMap, HeaderName, HeaderValue, Method};
use serde_json::Value;

use super::CredError;
use super::http::{CredClient, form, send, send_metadata};
use crate::sigv4::AwsCredentials;

/// Credentials with the time (Unix seconds) they stop being valid, when they do.
#[derive(Debug)]
pub(crate) struct Fetched {
    pub(crate) creds: AwsCredentials,
    pub(crate) expires: Option<i64>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn from_env() -> Option<AwsCredentials> {
    Some(AwsCredentials {
        access_key_id: env("AWS_ACCESS_KEY_ID")?,
        secret_access_key: antirion_core::Secret::new(env("AWS_SECRET_ACCESS_KEY")?),
        session_token: env("AWS_SESSION_TOKEN").map(antirion_core::Secret::new),
    })
}

/// The profile section of an INI-style credentials file.
fn ini_profile(text: &str, profile: &str) -> Option<AwsCredentials> {
    let mut in_section = false;
    let (mut id, mut secret, mut token) = (None, None, None);
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let name = name.trim();
            in_section = name == profile || name.strip_prefix("profile ").map(str::trim) == Some(profile);
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().to_owned();
        match k.trim() {
            "aws_access_key_id" => id = Some(v),
            "aws_secret_access_key" => secret = Some(v),
            "aws_session_token" => token = Some(v),
            _ => {}
        }
    }
    Some(AwsCredentials {
        access_key_id: id?,
        secret_access_key: antirion_core::Secret::new(secret?),
        session_token: token.map(antirion_core::Secret::new),
    })
}

async fn from_shared_file() -> Option<AwsCredentials> {
    let profile = env("AWS_PROFILE").unwrap_or_else(|| "default".to_owned());
    let path = env("AWS_SHARED_CREDENTIALS_FILE")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".aws").join("credentials"))
        })?;
    let text = tokio::fs::read_to_string(path).await.ok()?;
    ini_profile(&text, &profile)
}

pub(super) fn expiration(v: Option<&str>) -> Option<i64> {
    v.and_then(|s| UtcMicros::parse_rfc3339(s).ok())
        .map(|t| t.0 / 1_000_000)
}

/// Credentials in the JSON shape of the container and instance metadata endpoints.
fn json_credentials(status: u16, body: &[u8]) -> Result<Fetched, CredError> {
    if !(200..300).contains(&status) {
        return Err(CredError::Rejected(status));
    }
    let v: Value = serde_json::from_slice(body).map_err(|_| CredError::Malformed)?;
    let field = |n: &str| {
        v.get(n)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Ok(Fetched {
        creds: AwsCredentials {
            access_key_id: field("AccessKeyId").ok_or(CredError::Malformed)?,
            secret_access_key: field("SecretAccessKey")
                .map(antirion_core::Secret::new)
                .ok_or(CredError::Malformed)?,
            session_token: field("Token").map(antirion_core::Secret::new),
        },
        expires: expiration(v.get("Expiration").and_then(Value::as_str)),
    })
}

pub(super) fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag}>"))? + tag.len() + 2;
    let end = start + xml.get(start..)?.find(&format!("</{tag}>"))?;
    xml.get(start..end).map(|s| s.trim().to_owned())
}

async fn web_identity(
    client: &CredClient,
    tls: &TlsProfile,
    region: &str,
) -> Option<Result<Fetched, CredError>> {
    let token_file = env("AWS_WEB_IDENTITY_TOKEN_FILE")?;
    let role = env("AWS_ROLE_ARN")?;
    let session = env("AWS_ROLE_SESSION_NAME").unwrap_or_else(|| "antirion".to_owned());
    Some(
        async {
            let token = tokio::fs::read_to_string(&token_file)
                .await
                .map_err(|_| CredError::Unavailable)?;
            let url = format!("https://sts.{region}.amazonaws.com/");
            let body = form(&[
                ("Action", "AssumeRoleWithWebIdentity"),
                ("Version", "2011-06-15"),
                ("RoleArn", &role),
                ("RoleSessionName", &session),
                ("WebIdentityToken", token.trim()),
            ]);
            let mut h = HeaderMap::new();
            h.insert(
                http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/x-www-form-urlencoded"),
            );
            let reply = send(client, tls, Method::POST, &url, h, body).await?;
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
                    session_token: xml_tag(&xml, "SessionToken").map(antirion_core::Secret::new),
                },
                expires: expiration(xml_tag(&xml, "Expiration").as_deref()),
            })
        }
        .await,
    )
}

/// Whether a full container-credentials URI may be used: `https` anywhere, `http` only to a
/// loopback address or the ECS and EKS Pod Identity agents.
pub(crate) fn full_uri_allowed(uri: &str) -> bool {
    let Ok(u) = uri.parse::<http::Uri>() else {
        return false;
    };
    match u.scheme_str() {
        Some("https") => true,
        Some("http") => {
            let host = u
                .host()
                .unwrap_or_default()
                .trim_start_matches('[')
                .trim_end_matches(']');
            if host == "localhost" {
                return true;
            }
            match host.parse::<IpAddr>() {
                Ok(ip) if ip.is_loopback() => true,
                Ok(ip) => {
                    let agents: [IpAddr; 3] = [
                        IpAddr::from([169, 254, 170, 2]),
                        IpAddr::from([169, 254, 170, 23]),
                        IpAddr::from([0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x23]),
                    ];
                    agents.contains(&ip)
                }
                Err(_) => false,
            }
        }
        _ => false,
    }
}

async fn container(client: &CredClient, tls: &TlsProfile) -> Option<Result<Fetched, CredError>> {
    let url = match (
        env("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI"),
        env("AWS_CONTAINER_CREDENTIALS_FULL_URI"),
    ) {
        (Some(rel), _) => format!("http://169.254.170.2{rel}"),
        (None, Some(full)) => full,
        (None, None) => return None,
    };
    if !full_uri_allowed(&url) {
        return Some(Err(CredError::Endpoint));
    }
    let token = match env("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE") {
        Some(path) => tokio::fs::read_to_string(path)
            .await
            .ok()
            .map(|t| t.trim().to_owned()),
        None => env("AWS_CONTAINER_AUTHORIZATION_TOKEN"),
    };
    let mut headers = HeaderMap::new();
    if let Some(t) = token.and_then(|t| HeaderValue::from_str(&t).ok()) {
        headers.insert(AUTHORIZATION, t);
    }
    Some(
        match send_metadata(client, tls, Method::GET, &url, headers, Bytes::new()).await {
            Ok(r) => json_credentials(r.status, &r.body),
            Err(e) => Err(e),
        },
    )
}

async fn imds(client: &CredClient, tls: &TlsProfile) -> Result<Fetched, CredError> {
    let base =
        env("AWS_EC2_METADATA_SERVICE_ENDPOINT").unwrap_or_else(|| "http://169.254.169.254".to_owned());
    let base = base.trim_end_matches('/');
    let mut h = HeaderMap::new();
    h.insert(
        HeaderName::from_static("x-aws-ec2-metadata-token-ttl-seconds"),
        HeaderValue::from_static("21600"),
    );
    let token = send_metadata(
        client,
        tls,
        Method::PUT,
        &format!("{base}/latest/api/token"),
        h,
        Bytes::new(),
    )
    .await?;
    if token.status != 200 {
        return Err(CredError::Rejected(token.status));
    }
    let mut h = HeaderMap::new();
    let value = HeaderValue::from_bytes(&token.body).map_err(|_| CredError::Malformed)?;
    h.insert(HeaderName::from_static("x-aws-ec2-metadata-token"), value);
    let path = format!("{base}/latest/meta-data/iam/security-credentials/");
    let role = send_metadata(client, tls, Method::GET, &path, h.clone(), Bytes::new()).await?;
    if role.status != 200 {
        return Err(CredError::Rejected(role.status));
    }
    let name = String::from_utf8_lossy(&role.body)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if name.is_empty() {
        return Err(CredError::Unavailable);
    }
    let r = send_metadata(
        client,
        tls,
        Method::GET,
        &format!("{path}{name}"),
        h,
        Bytes::new(),
    )
    .await?;
    json_credentials(r.status, &r.body)
}

/// Resolves the default chain.
pub(crate) async fn chain(client: &CredClient, tls: &TlsProfile, region: &str) -> Result<Fetched, CredError> {
    if let Some(creds) = from_env() {
        return Ok(Fetched { creds, expires: None });
    }
    if let Some(creds) = from_shared_file().await {
        return Ok(Fetched { creds, expires: None });
    }
    if let Some(r) = web_identity(client, tls, region).await {
        return r;
    }
    if let Some(r) = container(client, tls).await {
        return r;
    }
    imds(client, tls).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_uri_host_rule() {
        assert!(full_uri_allowed("https://creds.example/v2"));
        assert!(full_uri_allowed("http://127.0.0.1:9000/v2"));
        assert!(full_uri_allowed("http://169.254.170.23/v1/credentials"));
        assert!(full_uri_allowed("http://[fd00:ec2::23]/v1/credentials"));
        assert!(!full_uri_allowed("http://creds.example/v2"));
        assert!(!full_uri_allowed("http://10.0.0.1/v2"));
    }

    #[test]
    fn credential_documents() {
        let f = json_credentials(
            200,
            br#"{"AccessKeyId":"ASIA1","SecretAccessKey":"s","Token":"t","Expiration":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(
            f.creds.session_token.as_ref().map(antirion_core::Secret::expose),
            Some("t")
        );
        assert_eq!(f.expires, Some(1_767_225_600));
        let ini =
            "[other]\naws_access_key_id=x\n[default]\naws_access_key_id = A\naws_secret_access_key = B\n";
        assert_eq!(ini_profile(ini, "default").unwrap().access_key_id, "A");
        assert_eq!(
            xml_tag("<R><AccessKeyId>AK</AccessKeyId></R>", "AccessKeyId").as_deref(),
            Some("AK")
        );
    }
}

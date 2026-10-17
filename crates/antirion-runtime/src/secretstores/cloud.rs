//! The cloud secret managers: AWS Secrets Manager (`GetSecretValue`, signed with SigV4), Google
//! Cloud Secret Manager (a version's `:access`) and Azure Key Vault (`GET /secrets/<name>`), with
//! the credential sources providers use.

use antirion_core::Clock;
use antirion_providers::TokenSource;
use antirion_providers::sigv4::{SigningInput, amz_date, sign};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Uri};
use serde_json::{Value, json};

use super::fetch::{Cx, FetchError, json as answer_json, send, sensitive};
use super::settings::{AwsSettings, AzureSettings, GcpSettings, StoreSettings};
use super::{Fetched, text_of};

const KEY_VAULT_API: &str = "7.4";

/// `<arn or name>[#<json field>]`; the region an ARN names.
fn aws_reference(reference: &str) -> Result<(&str, Option<&str>, Option<&str>), FetchError> {
    let (id, field) = match reference.split_once('#') {
        Some((id, field)) if !field.is_empty() => (id, Some(field)),
        Some(_) => return Err(FetchError::new("an aws-sm reference is <arn>[#<field>]")),
        None => (reference, None),
    };
    if id.is_empty() {
        return Err(FetchError::new("an aws-sm reference is <arn>[#<field>]"));
    }
    let region = id
        .strip_prefix("arn:")
        .and_then(|rest| rest.split(':').nth(2))
        .filter(|r| !r.is_empty());
    Ok((id, field, region))
}

pub(crate) async fn aws(
    cx: &Cx,
    s: &StoreSettings,
    a: &AwsSettings,
    reference: &str,
) -> Result<Fetched, FetchError> {
    let (id, field, arn_region) = aws_reference(reference)?;
    let region = arn_region
        .or(a.region.as_deref())
        .ok_or_else(|| FetchError::new("the reference is not an ARN and the store names no region"))?;
    let creds = match &a.assume_role {
        Some(role) => {
            cx.creds
                .aws_assumed(a.credentials.as_ref(), role, region, &cx.tls, &cx.policy)
                .await
        }
        None => {
            cx.creds
                .aws(a.credentials.as_ref(), region, &cx.tls, &cx.policy)
                .await
        }
    }
    .map_err(|e| FetchError::new(format!("no AWS credentials: {e}")))?;
    let url = match &a.endpoint {
        Some(e) => format!("{e}/"),
        None => format!("https://secretsmanager.{region}.amazonaws.com/"),
    };
    let uri: Uri = url
        .parse()
        .map_err(|_| FetchError::new("the store's URL is not valid"))?;
    let body = json!({"SecretId": id}).to_string();
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-amz-json-1.1"),
    );
    headers.insert(
        HeaderName::from_static("x-amz-target"),
        HeaderValue::from_static("secretsmanager.GetSecretValue"),
    );
    let date = amz_date(antirion_core::SystemClock.now());
    sign(
        &mut headers,
        &SigningInput {
            method: &Method::POST,
            uri: &uri,
            body: body.as_bytes(),
            region,
            service: "secretsmanager",
            amz_date: &date,
        },
        &creds,
    );
    let answer = send(cx, Method::POST, &url, headers, Bytes::from(body), s.timeout).await?;
    let reply = answer_json(&answer)?;
    let text = match (reply.get("SecretString"), reply.get("SecretBinary")) {
        (Some(Value::String(t)), _) => t.clone(),
        (_, Some(Value::String(b))) => STANDARD
            .decode(b)
            .ok()
            .and_then(|raw| String::from_utf8(raw).ok())
            .ok_or_else(|| FetchError::new("the secret's binary value is not UTF-8 text"))?,
        _ => return Err(FetchError::new("the answer has no secret value")),
    };
    let value = match field {
        None => text,
        Some(f) => {
            let parsed: Value = serde_json::from_str(&text)
                .map_err(|_| FetchError::new("the secret is not a JSON object"))?;
            parsed
                .get(f)
                .and_then(text_of)
                .ok_or_else(|| FetchError::new(format!("the secret has no text field {f}")))?
        }
    };
    Ok(Fetched { value, lease: None })
}

/// `projects/<p>/secrets/<s>[/versions/<v>]` as the version to access.
fn gcp_version(reference: &str) -> Result<String, FetchError> {
    let parts: Vec<&str> = reference.trim_matches('/').split('/').collect();
    match parts.as_slice() {
        ["projects", p, "secrets", s] if !p.is_empty() && !s.is_empty() => {
            Ok(format!("projects/{p}/secrets/{s}/versions/latest"))
        }
        ["projects", p, "secrets", s, "versions", v] if !p.is_empty() && !s.is_empty() && !v.is_empty() => {
            Ok(format!("projects/{p}/secrets/{s}/versions/{v}"))
        }
        _ => Err(FetchError::new(
            "a gcp-sm reference is projects/<project>/secrets/<secret>[/versions/<version>]",
        )),
    }
}

pub(crate) async fn gcp(
    cx: &Cx,
    s: &StoreSettings,
    g: &GcpSettings,
    reference: &str,
) -> Result<Fetched, FetchError> {
    let version = gcp_version(reference)?;
    let source = TokenSource::Gcp(std::sync::Arc::clone(&g.source));
    let token = cx
        .creds
        .token(&source, &cx.tls, &cx.policy)
        .await
        .map_err(|e| FetchError::new(format!("no Google Cloud token: {e}")))?;
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, sensitive(&format!("Bearer {token}"))?);
    let url = format!("{}/v1/{version}:access", g.endpoint);
    let answer = send(cx, Method::GET, &url, headers, Bytes::new(), s.timeout).await?;
    if answer.status == 401 {
        cx.creds.invalidate(&source, &token);
    }
    let reply = answer_json(&answer)?;
    let value = reply
        .pointer("/payload/data")
        .and_then(Value::as_str)
        .and_then(|b| STANDARD.decode(b).ok())
        .and_then(|raw| String::from_utf8(raw).ok())
        .ok_or_else(|| FetchError::new("the answer has no text payload"))?;
    Ok(Fetched { value, lease: None })
}

/// `<vault>/<name>[/<version>]`.
fn azure_reference(reference: &str) -> Result<(&str, &str, Option<&str>), FetchError> {
    let ok = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let parts: Vec<&str> = reference.split('/').collect();
    match parts.as_slice() {
        [vault, name] if ok(vault) && ok(name) => Ok((vault, name, None)),
        [vault, name, version] if ok(vault) && ok(name) && ok(version) => Ok((vault, name, Some(version))),
        _ => Err(FetchError::new(
            "an azure-kv reference is <vault>/<name>[/<version>]",
        )),
    }
}

pub(crate) async fn azure(
    cx: &Cx,
    s: &StoreSettings,
    a: &AzureSettings,
    reference: &str,
) -> Result<Fetched, FetchError> {
    let (vault, name, version) = azure_reference(reference)?;
    let source = TokenSource::Azure(std::sync::Arc::clone(&a.source));
    let token = cx
        .creds
        .token(&source, &cx.tls, &cx.policy)
        .await
        .map_err(|e| FetchError::new(format!("no Azure AD token: {e}")))?;
    let base = match &a.endpoint {
        Some(e) => format!("{e}/{vault}"),
        None => format!("https://{vault}.{}", a.vault_suffix),
    };
    let path = match version {
        Some(v) => format!("secrets/{name}/{v}"),
        None => format!("secrets/{name}"),
    };
    let url = format!("{base}/{path}?api-version={KEY_VAULT_API}");
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, sensitive(&format!("Bearer {token}"))?);
    let answer = send(cx, Method::GET, &url, headers, Bytes::new(), s.timeout).await?;
    if answer.status == 401 {
        cx.creds.invalidate(&source, &token);
    }
    let reply = answer_json(&answer)?;
    let value = reply
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| FetchError::new("the answer has no value"))?
        .to_owned();
    Ok(Fetched { value, lease: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aws_references_take_the_region_of_an_arn() {
        let arn = "arn:aws:secretsmanager:eu-west-1:123456789012:secret:gw/openai-AbCdEf";
        assert_eq!(aws_reference(arn).unwrap(), (arn, None, Some("eu-west-1")));
        assert_eq!(
            aws_reference(&format!("{arn}#api_key")).unwrap(),
            (arn, Some("api_key"), Some("eu-west-1"))
        );
        assert_eq!(aws_reference("gw/openai").unwrap(), ("gw/openai", None, None));
        assert!(aws_reference("x#").is_err());
        assert!(aws_reference("#f").is_err());
    }

    #[test]
    fn gcp_references_default_to_the_latest_version() {
        assert_eq!(
            gcp_version("projects/p/secrets/s").unwrap(),
            "projects/p/secrets/s/versions/latest"
        );
        assert_eq!(
            gcp_version("projects/p/secrets/s/versions/7").unwrap(),
            "projects/p/secrets/s/versions/7"
        );
        assert!(gcp_version("p/s").is_err());
    }

    #[test]
    fn azure_references_name_a_vault_and_a_secret() {
        assert_eq!(
            azure_reference("kv-prod/openai").unwrap(),
            ("kv-prod", "openai", None)
        );
        assert_eq!(
            azure_reference("kv-prod/openai/0123abcd").unwrap(),
            ("kv-prod", "openai", Some("0123abcd"))
        );
        assert!(azure_reference("kv.evil/x").is_err());
        assert!(azure_reference("only").is_err());
    }
}

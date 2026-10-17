//! `antirion auth login` and `logout`: an admin token for the command line through the gateway's
//! device authorization grant, kept per admin URL in the user's credentials file.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use bytes::Bytes;
use http::{Method, Request, Uri};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};

/// Where a sign-in goes and how.
#[derive(Debug)]
pub(crate) struct Target {
    /// The admin listener's base URL (`http://127.0.0.1:8081`).
    pub(crate) admin_url: String,
    /// A PEM bundle trusted besides the public roots.
    pub(crate) ca_file: Option<PathBuf>,
}

/// One answer of the Admin API.
struct Answer {
    status: u16,
    body: Value,
}

fn credentials_path() -> anyhow::Result<PathBuf> {
    if let Some(p) = std::env::var_os("ANTIRION_CREDENTIALS") {
        return Ok(PathBuf::from(p));
    }
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => {
            PathBuf::from(std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?).join(".config")
        }
    };
    Ok(base.join("antirion").join("credentials.json"))
}

fn read_credentials(path: &Path) -> BTreeMap<String, Value> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Writes the file readable by its owner only.
fn write_credentials(path: &Path, all: &BTreeMap<String, Value>) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    f.write_all(&serde_json::to_vec_pretty(all)?)?;
    f.write_all(b"\n")?;
    Ok(())
}

fn tls_config(ca_file: Option<&Path>) -> anyhow::Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = ca_file {
        use rustls::pki_types::CertificateDer;
        use rustls::pki_types::pem::PemObject;
        let pem = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        for cert in CertificateDer::pem_slice_iter(&pem) {
            roots.add(cert?)?;
        }
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    Ok(Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

/// One request to the Admin API over HTTP/1.1 (TLS for `https`), JSON both ways.
async fn call(
    target: &Target,
    method: Method,
    path: &str,
    body: Option<&Value>,
    bearer: Option<&str>,
) -> anyhow::Result<Answer> {
    let base: Uri = target.admin_url.trim_end_matches('/').parse()?;
    let host = base
        .host()
        .ok_or_else(|| anyhow!("the admin URL has no host"))?
        .to_owned();
    let https = match base.scheme_str() {
        Some("https") => true,
        Some("http") => false,
        _ => bail!("the admin URL must be http:// or https://"),
    };
    let port = base.port_u16().unwrap_or(if https { 443 } else { 80 });
    let prefix = base.path().trim_end_matches('/');
    let uri: Uri = format!("{prefix}{path}").parse()?;
    let tcp = tokio::net::TcpStream::connect((host.trim_matches(['[', ']']), port))
        .await
        .with_context(|| format!("connecting to {}", target.admin_url))?;
    let payload = body.map(|b| Bytes::from(b.to_string())).unwrap_or_default();
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(
            http::header::HOST,
            base.authority()
                .map_or(host.as_str(), http::uri::Authority::as_str),
        )
        .header(http::header::CONNECTION, "close")
        .header(http::header::ACCEPT, "application/json")
        .header(
            http::header::USER_AGENT,
            concat!("antirion-cli/", env!("CARGO_PKG_VERSION")),
        );
    if body.is_some() {
        req = req.header(http::header::CONTENT_TYPE, "application/json");
    }
    if let Some(t) = bearer {
        req = req.header(http::header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = req.body(Full::new(payload))?;
    let mut tasks = tokio::task::JoinSet::new();
    let resp = if https {
        let name = rustls::pki_types::ServerName::try_from(host.trim_matches(['[', ']']).to_owned())?;
        let tls = tokio_rustls::TlsConnector::from(tls_config(target.ca_file.as_deref())?)
            .connect(name, tcp)
            .await?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls)).await?;
        tasks.spawn(async move {
            let _ = conn.await;
        });
        sender.send_request(req).await?
    } else {
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tcp)).await?;
        tasks.spawn(async move {
            let _ = conn.await;
        });
        sender.send_request(req).await?
    };
    let status = resp.status().as_u16();
    let bytes = resp.into_body().collect().await?.to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Ok(Answer { status, body })
}

fn say(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{text}");
}

fn error_text(a: &Answer) -> String {
    a.body
        .pointer("/error/message")
        .or_else(|| a.body.get("error_description"))
        .and_then(Value::as_str)
        .map_or_else(|| format!("status {}", a.status), str::to_owned)
}

/// Runs the device flow and keeps the token.
pub(crate) async fn login(target: &Target, sso: bool, organization: Option<String>) -> anyhow::Result<()> {
    let mut start = json!({"client_id": "antirion-cli"});
    if sso {
        start["sso"] = Value::Bool(true);
    }
    if let Some(o) = organization {
        start["organization"] = Value::from(o);
    }
    let a = call(target, Method::POST, "/admin/v1/auth/device", Some(&start), None).await?;
    match a.status {
        200 => {}
        404 => bail!("this gateway does not offer sign-in from the command line"),
        _ => bail!("the gateway refused to start a sign-in: {}", error_text(&a)),
    }
    let field = |name: &str| {
        a.body
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let device_code = field("device_code");
    let user_code = field("user_code");
    say(&format!(
        "Open {} and enter the code {user_code}",
        field("verification_uri")
    ));
    say(&format!("(or open {})", field("verification_uri_complete")));
    say("Waiting for approval...");
    let mut interval = a.body.get("interval").and_then(Value::as_u64).unwrap_or(5).max(1);
    let expires = a.body.get("expires_in").and_then(Value::as_u64).unwrap_or(600);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(expires);
    let poll = json!({
        "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
        "device_code": device_code,
    });
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if tokio::time::Instant::now() >= deadline {
            bail!("the code expired before the sign-in was approved");
        }
        let a = call(
            target,
            Method::POST,
            "/admin/v1/auth/device/token",
            Some(&poll),
            None,
        )
        .await?;
        if a.status == 200 {
            return keep(target, &a.body);
        }
        match a.body.get("error").and_then(Value::as_str) {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some("access_denied") => bail!("the sign-in was denied"),
            Some("expired_token") => bail!("the code expired before the sign-in was approved"),
            _ => bail!("the sign-in failed: {}", error_text(&a)),
        }
    }
}

fn keep(target: &Target, token: &Value) -> anyhow::Result<()> {
    let path = credentials_path()?;
    let mut all = read_credentials(&path);
    let user = token
        .get("user")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let ttl = token.get("expires_in").and_then(Value::as_u64).unwrap_or(0);
    all.insert(
        target.admin_url.trim_end_matches('/').to_owned(),
        json!({
            "token": token.get("access_token"),
            "kid": token.get("kid"),
            "user": user,
        }),
    );
    write_credentials(&path, &all)?;
    say(&format!(
        "Signed in as {user}; the token is kept in {} and expires in {}.",
        path.display(),
        antirion_config::scalar::format_duration(Duration::from_secs(ttl))
    ));
    Ok(())
}

/// Revokes the kept token and forgets it.
pub(crate) async fn logout(target: &Target) -> anyhow::Result<()> {
    let path = credentials_path()?;
    let mut all = read_credentials(&path);
    let key = target.admin_url.trim_end_matches('/').to_owned();
    let Some(entry) = all.remove(&key) else {
        say("Not signed in to this gateway.");
        return Ok(());
    };
    if let (Some(token), Some(kid)) = (
        entry.get("token").and_then(Value::as_str),
        entry.get("kid").and_then(Value::as_str),
    ) {
        let a = call(
            target,
            Method::DELETE,
            &format!("/admin/v1/auth/me/tokens/{kid}"),
            None,
            Some(token),
        )
        .await?;
        if !matches!(a.status, 204 | 401 | 404) {
            bail!("the gateway did not revoke the token: {}", error_text(&a));
        }
    }
    write_credentials(&path, &all)?;
    say("Signed out.");
    Ok(())
}

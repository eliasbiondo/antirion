//! Scrubbing of upstream-derived text before it reaches any sink: the secrets the gateway holds (in
//! their encoded forms too), credential-shaped tokens, sensitive URL query values and userinfo, and
//! internal hosts.

use std::fmt::Write as _;
use std::net::IpAddr;
use std::sync::{Arc, LazyLock, OnceLock};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use regex::{Captures, Regex};

/// What one provider's upstream text is scrubbed of. The encoded forms of the secrets are derived
/// on the first use: most responses carry no text to scrub.
#[derive(Clone, Debug, Default)]
pub(crate) struct Scrub {
    secrets: Vec<String>,
    forms: OnceLock<Vec<String>>,
    hosts: Arc<Vec<String>>,
}

static CREDENTIALS: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(concat!(
        r"sk-antirion-[A-Za-z0-9_\-]+|ak-antirion-[A-Za-z0-9_\-]+",
        r"|sk-[A-Za-z0-9_\-]{3,}\*+[A-Za-z0-9_\-]+|sk-[A-Za-z0-9_\-]{8,}",
        r"|AKIA[0-9A-Z]{16}|ASIA[0-9A-Z]{16}|AIza[0-9A-Za-z_\-]{35}",
        r"|eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+",
        r"|(?i:bearer)\s+[A-Za-z0-9._~+/\-]+=*",
        r"|(?i:(?:api[_-]?key|apikey|access_token|token|secret|password))\s*[=:]\s*[^\s,;&]+",
    ))
    .ok()
});

static QUERY: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)([?&](?:key|api_key|apikey|token|access_token|sig|signature|code|x-amz-signature|x-amz-credential",
        r"|x-amz-security-token)=)[^&\s#]+",
    ))
    .ok()
});

static USERINFO: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"(://)[^/@\s]+@").ok());

static IPV4: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\b\d{1,3}(?:\.\d{1,3}){3}\b").ok());

static INTERNAL_NAME: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:[a-z0-9-]+\.)*[a-z0-9-]+\.(?:internal|local|localhost|corp|lan|intranet|svc|home\.arpa)\b",
    )
    .ok()
});

fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.octets()[0] == 100 && (64..128).contains(&v.octets()[1])
        }
        IpAddr::V6(v) => v.is_loopback() || v.is_unspecified() || (v.segments()[0] & 0xfe00) == 0xfc00,
    }
}

/// The encoded forms of a secret the upstream might echo.
fn forms(secret: &str) -> Vec<String> {
    let mut out = vec![secret.to_owned()];
    if let Ok(json) = serde_json::to_string(secret) {
        out.push(json.trim_matches('"').to_owned());
    }
    let mut url = String::with_capacity(secret.len());
    for b in secret.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            url.push(char::from(b));
        } else {
            let _ = write!(url, "%{b:02X}");
        }
    }
    out.push(url);
    for e in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        out.push(e.encode(secret.as_bytes()));
    }
    out.sort_by_key(|f| std::cmp::Reverse(f.len()));
    out.dedup();
    out
}

fn replace(re: &LazyLock<Option<Regex>>, text: &str, with: impl Fn(&Captures<'_>) -> String) -> String {
    match re.as_ref() {
        Some(r) => r.replace_all(text, |c: &Captures<'_>| with(c)).into_owned(),
        None => text.to_owned(),
    }
}

impl Scrub {
    pub(crate) fn new(secrets: &[String], hosts: Arc<Vec<String>>) -> Scrub {
        Scrub {
            secrets: secrets.iter().filter(|s| s.len() >= 4).cloned().collect(),
            forms: OnceLock::new(),
            hosts,
        }
    }

    /// The scrubbed text.
    pub(crate) fn apply(&self, text: &str) -> String {
        let mut out = text.to_owned();
        let forms = self
            .forms
            .get_or_init(|| self.secrets.iter().flat_map(|s| forms(s)).collect());
        for s in forms {
            out = out.replace(s.as_str(), "[REDACTED]");
        }
        out = replace(&USERINFO, &out, |c| format!("{}[REDACTED]@", &c[1]));
        out = replace(&QUERY, &out, |c| format!("{}[REDACTED]", &c[1]));
        out = replace(&CREDENTIALS, &out, |_| "[REDACTED]".to_owned());
        for h in self.hosts.iter() {
            out = out.replace(h.as_str(), "[REDACTED_HOST]");
        }
        out = replace(&INTERNAL_NAME, &out, |_| "[REDACTED_HOST]".to_owned());
        replace(&IPV4, &out, |c| match c[0].parse::<IpAddr>() {
            Ok(ip) if private_ip(ip) => "[REDACTED_HOST]".to_owned(),
            _ => c[0].to_owned(),
        })
    }
}

/// The non-public hosts of the configured upstream URLs, `host:port` first, then the host alone.
#[must_use]
pub(crate) fn configured_hosts(urls: impl Iterator<Item = String>) -> Vec<String> {
    const PUBLIC: &[&str] = &[
        "api.openai.com",
        "api.anthropic.com",
        "generativelanguage.googleapis.com",
        "aiplatform.googleapis.com",
        "oauth2.googleapis.com",
    ];
    let mut out = Vec::new();
    for u in urls {
        let Ok(uri) = u.parse::<http::Uri>() else { continue };
        let Some(host) = uri.host() else { continue };
        let public = PUBLIC.contains(&host)
            || host.ends_with(".googleapis.com")
            || (host.starts_with("bedrock-runtime.") && host.ends_with(".amazonaws.com"));
        if public {
            continue;
        }
        if let Some(port) = uri.port_u16() {
            out.push(format!("{host}:{port}"));
        }
        out.push(host.to_owned());
    }
    out.sort_by_key(|h| std::cmp::Reverse(h.len()));
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rule_applies() {
        let s = Scrub::new(
            &["sk-upstream-p".to_owned()],
            Arc::new(configured_hosts(
                ["http://127.0.0.1:9000/v1".to_owned()].into_iter(),
            )),
        );
        let text = "key sk-upstream-p c2stdXBzdHJlYW0tcA== masked sk-abc1****wxyz; see https://vllm.internal.corp:8000/v1?token=t0k&x=1; peer 10.2.3.4; at http://127.0.0.1:9000/x; public 8.8.8.8";
        let out = s.apply(text);
        for gone in [
            "sk-upstream-p",
            "c2stdXBzdHJlYW0tcA",
            "sk-abc1",
            "vllm.internal.corp",
            "t0k",
            "10.2.3.4",
            "127.0.0.1",
        ] {
            assert!(!out.contains(gone), "{gone} in {out}");
        }
        assert!(out.contains("8.8.8.8") && out.contains("[REDACTED_HOST]"));
        assert_eq!(s.apply("use Bearer abc.def"), "use [REDACTED]");
    }
}

//! Secret detectors: case-sensitive patterns that must not touch a key character on either side,
//! and the PEM private-key block found by its armour lines.

use std::sync::LazyLock;

use regex::Regex;

use crate::pattern::Span;

/// A credential detector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detector {
    OpenaiKey,
    AnthropicKey,
    AwsAccessKey,
    GithubToken,
    SlackToken,
    GoogleApiKey,
    StripeKey,
    Jwt,
    PemPrivateKey,
}

pub(crate) const ALL: [Detector; 9] = [
    Detector::OpenaiKey,
    Detector::AnthropicKey,
    Detector::AwsAccessKey,
    Detector::GithubToken,
    Detector::SlackToken,
    Detector::GoogleApiKey,
    Detector::StripeKey,
    Detector::Jwt,
    Detector::PemPrivateKey,
];

const PEM_BODY_MAX: usize = 16384;

static PATTERNS: LazyLock<Vec<(Detector, Regex)>> = LazyLock::new(|| {
    [
        (Detector::AnthropicKey, r"sk-ant-[A-Za-z0-9_-]{20,200}"),
        (
            Detector::OpenaiKey,
            r"sk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_-]{20,200}",
        ),
        (Detector::AwsAccessKey, r"(?:AKIA|ASIA)[0-9A-Z]{16}"),
        (Detector::GithubToken, r"gh[pousr]_[A-Za-z0-9]{36,251}"),
        (Detector::SlackToken, r"xox[abposr]-[A-Za-z0-9-]{10,250}"),
        (Detector::GoogleApiKey, r"AIza[0-9A-Za-z_-]{35}"),
        (Detector::StripeKey, r"[rs]k_(?:live|test)_[0-9A-Za-z]{16,247}"),
        (
            Detector::Jwt,
            r"eyJ[A-Za-z0-9_-]{5,}\.eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{10,}",
        ),
    ]
    .into_iter()
    .filter_map(|(d, p)| Regex::new(p).ok().map(|r| (d, r)))
    .collect()
});

static PEM_BEGIN: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"-----BEGIN [A-Z ]{0,20}PRIVATE KEY-----").ok());
static PEM_END: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"-----END [A-Z ]{0,20}PRIVATE KEY-----").ok());

fn key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

impl Detector {
    pub(crate) fn parse(s: &str) -> Option<Detector> {
        ALL.into_iter().find(|d| d.name() == s)
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Detector::OpenaiKey => "openai_key",
            Detector::AnthropicKey => "anthropic_key",
            Detector::AwsAccessKey => "aws_access_key",
            Detector::GithubToken => "github_token",
            Detector::SlackToken => "slack_token",
            Detector::GoogleApiKey => "google_api_key",
            Detector::StripeKey => "stripe_key",
            Detector::Jwt => "jwt",
            Detector::PemPrivateKey => "pem_private_key",
        }
    }

    /// The longest match in code points; `None` for `jwt`, which is unbounded.
    #[must_use]
    pub const fn max_len(self) -> Option<usize> {
        match self {
            Detector::OpenaiKey => Some(213),
            Detector::AnthropicKey => Some(207),
            Detector::AwsAccessKey => Some(20),
            Detector::GithubToken | Detector::SlackToken | Detector::StripeKey => Some(255),
            Detector::GoogleApiKey => Some(39),
            Detector::Jwt => None,
            Detector::PemPrivateKey => Some(16448),
        }
    }

    fn span(self, start: usize, end: usize) -> Span {
        Span {
            start,
            end,
            label: self.name().to_ascii_uppercase(),
        }
    }

    /// Every occurrence of this credential in `text`.
    pub(crate) fn spans(self, text: &str) -> Vec<Span> {
        if self == Detector::PemPrivateKey {
            return pem(text).into_iter().map(|(s, e)| self.span(s, e)).collect();
        }
        let Some((_, re)) = PATTERNS.iter().find(|(d, _)| *d == self) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut at = 0;
        while at <= text.len() {
            let Some(m) = re.find_at(text, at) else { break };
            let before = text.get(..m.start()).and_then(|s| s.chars().next_back());
            let after = text.get(m.end()..).and_then(|s| s.chars().next());
            if before.is_some_and(key_char) || after.is_some_and(key_char) {
                at = crate::pattern::next_boundary(text, m.start());
                continue;
            }
            out.push(self.span(m.start(), m.end()));
            at = m.end();
        }
        out
    }
}

/// The settings of a `secrets` guardrail.
#[derive(Clone, Debug, PartialEq)]
pub struct SecretRules {
    pub detectors: Vec<Detector>,
    /// Also detect high-entropy tokens of at least this many bits per character.
    pub min_entropy: Option<f64>,
}

/// The shortest token the entropy detector considers.
const MIN_TOKEN: usize = 20;
/// The label of a match that is a well-formed gateway virtual key.
pub const GATEWAY_KEY: &str = "ANTIRION_KEY";
/// The label of a high-entropy token.
pub const HIGH_ENTROPY: &str = "HIGH_ENTROPY";

fn base64url(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// The Shannon entropy of an ASCII token, in bits per character.
fn entropy(token: &[u8]) -> f64 {
    let mut counts = [0u32; 128];
    for b in token {
        if let Some(c) = counts.get_mut(usize::from(*b)) {
            *c += 1;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let n = token.len() as f64;
    counts
        .iter()
        .filter(|c| **c > 0)
        .map(|c| {
            let p = f64::from(*c) / n;
            -p * p.log2()
        })
        .sum()
}

/// Runs of at least [`MIN_TOKEN`] base64url characters whose entropy reaches `min`.
fn entropic(text: &str, min: f64) -> Vec<Span> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes.get(i).copied().is_some_and(base64url) {
            i += 1;
            continue;
        }
        let start = i;
        while bytes.get(i).copied().is_some_and(base64url) {
            i += 1;
        }
        if let Some(token) = bytes.get(start..i)
            && token.len() >= MIN_TOKEN
            && entropy(token) >= min
        {
            out.push(Span {
                start,
                end: i,
                label: HIGH_ENTROPY.to_owned(),
            });
        }
    }
    out
}

/// The key id of a well-formed gateway virtual key.
#[must_use]
pub fn gateway_kid(matched: &str) -> Option<String> {
    let c = antirion_identity::keyfmt::Credential::parse(matched)?;
    (c.plane() == antirion_identity::keyfmt::Plane::Data).then(|| c.kid().to_owned())
}

impl SecretRules {
    /// Every credential in `text`; a match that is a gateway virtual key is labelled as one.
    #[must_use]
    pub fn spans(&self, text: &str) -> Vec<Span> {
        let mut out: Vec<Span> = self.detectors.iter().flat_map(|d| d.spans(text)).collect();
        if let Some(min) = self.min_entropy {
            out.extend(entropic(text, min));
        }
        for s in &mut out {
            if text.get(s.start..s.end).is_some_and(|m| gateway_kid(m).is_some()) {
                GATEWAY_KEY.clone_into(&mut s.label);
            }
        }
        out
    }

    /// The longest match in code points; `None` when unbounded.
    #[must_use]
    pub fn max_len(&self) -> Option<usize> {
        if self.min_entropy.is_some() {
            return None;
        }
        self.detectors
            .iter()
            .map(|d| d.max_len())
            .try_fold(0, |m, l| l.map(|l| m.max(l)))
    }
}

/// PEM private-key blocks: an armour line, at most 16 KiB of body, the matching end line.
fn pem(text: &str) -> Vec<(usize, usize)> {
    let (Some(begin), Some(end)) = (PEM_BEGIN.as_ref(), PEM_END.as_ref()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(b) = text.get(at..).and_then(|t| begin.find(t)) {
        let start = at + b.start();
        let body_from = at + b.end();
        let window_end = text
            .get(body_from..)
            .map(|t| {
                t.char_indices()
                    .nth(PEM_BODY_MAX)
                    .map_or(text.len(), |(i, _)| body_from + i)
            })
            .unwrap_or(text.len());
        let found = text
            .get(body_from..)
            .and_then(|t| end.find(t))
            .filter(|e| body_from + e.start() <= window_end);
        match found {
            Some(e) => {
                out.push((start, body_from + e.end()));
                at = body_from + e.end();
            }
            None => at = body_from,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detectors_find_their_credentials_within_boundaries() {
        let cases = [
            (Detector::AwsAccessKey, "AKIAIOSFODNN7EXAMPLE"),
            (Detector::OpenaiKey, "sk-proj-Ab3Ab3Ab3Ab3Ab3Ab3Ab3Ab3Ab3Ab3"),
            (
                Detector::AnthropicKey,
                "sk-ant-api03-Zx9_Zx9_Zx9_Zx9_Zx9_Zx9_Zx9_Zx9_",
            ),
            (Detector::SlackToken, "xoxb-1234567890-abcdefghij"),
            (Detector::StripeKey, "sk_live_4eC39HqLyjWDarjtT1zdp7dc"),
        ];
        for (d, v) in cases {
            let text = format!("use {v} here");
            let spans = d.spans(&text);
            assert_eq!(spans.len(), 1, "{d:?}");
            assert_eq!(&text[spans[0].start..spans[0].end], v);
        }
        for t in [
            "xAKIAIOSFODNN7EXAMPLE",
            "AKIAIOSFODNN7EXAMPLE_",
            "AKIAIOSFODNN7EXAMPL",
        ] {
            assert!(
                Detector::AwsAccessKey.spans(&format!("k {t} k")).is_empty(),
                "{t}"
            );
        }
    }

    #[test]
    fn high_entropy_tokens_and_gateway_keys_are_found() {
        let rules = SecretRules {
            detectors: vec![Detector::OpenaiKey],
            min_entropy: Some(3.5),
        };
        let text = "token Zq8xK2pW9mN4vB7cR1tY6uJ3 and internationalization_rules ok";
        let spans = rules.spans(text);
        assert_eq!(spans.len(), 1);
        assert_eq!(&text[spans[0].start..spans[0].end], "Zq8xK2pW9mN4vB7cR1tY6uJ3");
        assert_eq!(spans[0].label, HIGH_ENTROPY);
        assert!(entropy(b"aaaaaaaaaaaaaaaaaaaaaaaa") < 0.1);
        let key =
            antirion_identity::keyfmt::Credential::generate(antirion_identity::keyfmt::Plane::Data).unwrap();
        let leak = format!("my key is {} thanks", key.expose());
        let found = rules.spans(&leak);
        assert!(
            !found.is_empty() && found.iter().all(|s| s.label == GATEWAY_KEY),
            "{found:?}"
        );
        assert_eq!(gateway_kid(key.expose()).as_deref(), Some(key.kid()));
        let mut broken = key.expose().to_owned();
        let last = broken.pop().unwrap();
        broken.push(if last == 'x' { 'y' } else { 'x' });
        assert!(gateway_kid(&broken).is_none(), "a wrong checksum is not a key");
    }

    #[test]
    fn pem_blocks_span_the_armour() {
        let text = "a\n-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----\nb";
        let spans = Detector::PemPrivateKey.spans(text);
        assert_eq!(spans.len(), 1);
        assert_eq!(&text[spans[0].start..spans[0].end], &text[2..text.len() - 2]);
    }
}

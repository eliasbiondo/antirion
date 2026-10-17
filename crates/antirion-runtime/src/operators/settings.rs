//! The console's operator-identity settings and the outgoing mail settings, compiled from a
//! resolved document.

use std::time::Duration;

use antirion_config::console_views::{ConsoleAuthView, SmtpView};
use antirion_config::scalar::parse_duration;
use antirion_config::views::view;
use antirion_identity::password::Policy;
use serde_json::Value;

const DAY: u64 = 86_400;

/// A second factor a local account may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactorKind {
    Totp,
    Webauthn,
}

impl FactorKind {
    #[must_use]
    pub fn parse(s: &str) -> Option<FactorKind> {
        match s {
            "totp" => Some(FactorKind::Totp),
            "webauthn" => Some(FactorKind::Webauthn),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FactorKind::Totp => "totp",
            FactorKind::Webauthn => "webauthn",
        }
    }
}

/// `auth.admin.session.trusted_device`, when enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrustedDevice {
    pub max_age: Duration,
    pub idle: Duration,
}

/// `auth.admin.device_authorization`, when enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceAuthorization {
    pub code_ttl: Duration,
    pub poll_interval: Duration,
    pub token_ttl: Duration,
    pub max_starts_per_minute: u32,
}

/// How mail reaches its server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailSettings {
    pub host: String,
    pub port: u16,
    pub security: antirion_mail::Security,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: String,
    pub reply_to: Option<String>,
    pub timeout: Duration,
    pub per_minute: u32,
}

/// The console's identity settings of one generation.
#[derive(Clone, Debug)]
pub struct ConsoleSettings {
    pub local: bool,
    pub policy: Policy,
    /// When a password older than this must be changed at the next sign-in.
    pub password_max_age: Option<Duration>,
    pub factors: Vec<FactorKind>,
    pub rp_name: String,
    pub rp_id: Option<String>,
    pub recovery_codes: usize,
    pub sso_required_domains: Vec<String>,
    pub lockout_failures: u32,
    pub lockout_window: Duration,
    pub lockout_duration: Duration,
    pub reset_ttl: Duration,
    pub invitation_ttl: Duration,
    pub show_version: bool,
    pub ticker: bool,
    pub personal_tokens: bool,
    pub personal_max_lifetime: Duration,
    pub trusted: Option<TrustedDevice>,
    pub throttle_failures: u32,
    pub throttle_window: Duration,
    pub mail: Option<MailSettings>,
    pub device: Option<DeviceAuthorization>,
}

impl Default for ConsoleSettings {
    fn default() -> ConsoleSettings {
        ConsoleSettings::of(&Value::Null)
    }
}

fn dur(s: Option<&String>, default: Duration) -> Duration {
    s.and_then(|s| parse_duration(s)).unwrap_or(default)
}

impl ConsoleSettings {
    /// The settings of a resolved document (secrets already expanded).
    #[must_use]
    pub fn of(resolved: &Value) -> ConsoleSettings {
        let v: ConsoleAuthView = resolved
            .pointer("/auth/admin")
            .map(view)
            .and_then(Result::ok)
            .unwrap_or_default();
        let local = &v.local;
        let pp = &local.password_policy;
        let defaults = Policy::default();
        let factors: Vec<FactorKind> = local.mfa.factors.as_ref().map_or_else(
            || vec![FactorKind::Totp, FactorKind::Webauthn],
            |f| f.iter().filter_map(|s| FactorKind::parse(s)).collect(),
        );
        let td = &v.session.trusted_device;
        ConsoleSettings {
            local: local.enabled,
            policy: Policy {
                min_length: pp.min_length.unwrap_or(defaults.min_length),
                max_length: pp.max_length.unwrap_or(defaults.max_length),
                deny_common: pp.deny_common.unwrap_or(defaults.deny_common),
                history: pp.history.unwrap_or(defaults.history),
            },
            password_max_age: pp
                .max_age
                .as_deref()
                .filter(|s| *s != "none")
                .and_then(parse_duration),
            factors,
            rp_name: local
                .mfa
                .webauthn
                .rp_name
                .clone()
                .unwrap_or_else(|| "Antirion".to_owned()),
            rp_id: local.mfa.webauthn.rp_id.clone(),
            recovery_codes: local.mfa.recovery_codes.unwrap_or(10),
            sso_required_domains: local
                .sso_required_domains
                .iter()
                .map(|d| d.to_ascii_lowercase())
                .collect(),
            lockout_failures: local.lockout.max_failures.unwrap_or(10),
            lockout_window: dur(local.lockout.window.as_ref(), Duration::from_secs(900)),
            lockout_duration: dur(local.lockout.duration.as_ref(), Duration::from_secs(900)),
            reset_ttl: dur(local.reset_token_ttl.as_ref(), Duration::from_secs(1800)),
            invitation_ttl: dur(v.invitations.ttl.as_ref(), Duration::from_secs(7 * DAY)),
            show_version: v.login_page.show_version,
            ticker: v.login_page.ticker.enabled,
            personal_tokens: v.personal_tokens.enabled,
            personal_max_lifetime: dur(
                v.personal_tokens.max_lifetime.as_ref(),
                Duration::from_secs(90 * DAY),
            ),
            trusted: td.enabled.then(|| TrustedDevice {
                max_age: dur(td.max_age.as_ref(), Duration::from_secs(30 * DAY)),
                idle: dur(td.idle_timeout.as_ref(), Duration::from_secs(7 * DAY)),
            }),
            throttle_failures: v.login_throttle.max_failures.unwrap_or(10),
            throttle_window: dur(v.login_throttle.window.as_ref(), Duration::from_secs(60)),
            mail: mail(resolved),
            device: v.device_authorization.enabled.then(|| {
                let d = &v.device_authorization;
                DeviceAuthorization {
                    code_ttl: dur(d.code_ttl.as_ref(), Duration::from_secs(600)),
                    poll_interval: dur(d.poll_interval.as_ref(), Duration::from_secs(5)),
                    token_ttl: dur(d.token_ttl.as_ref(), Duration::from_secs(12 * 3600)),
                    max_starts_per_minute: d.max_starts_per_minute.unwrap_or(10),
                }
            }),
        }
    }

    /// Whether `kind` may be enrolled and used.
    #[must_use]
    pub fn allows(&self, kind: FactorKind) -> bool {
        self.factors.contains(&kind)
    }
}

fn mail(resolved: &Value) -> Option<MailSettings> {
    let s: SmtpView = resolved.get("smtp").map(view).and_then(Result::ok)?;
    if !s.enabled {
        return None;
    }
    Some(MailSettings {
        host: s.host?,
        port: s.port.unwrap_or(587),
        security: match s.tls.as_deref() {
            Some("tls") => antirion_mail::Security::Tls,
            Some("none") => antirion_mail::Security::None,
            _ => antirion_mail::Security::StartTls,
        },
        username: s.username,
        password: s.password,
        from: s.from?,
        reply_to: s.reply_to,
        timeout: dur(s.timeout.as_ref(), Duration::from_secs(10)),
        per_minute: s.max_messages_per_minute.unwrap_or(60),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let d = ConsoleSettings::default();
        assert!(!d.local && d.mail.is_none() && d.trusted.is_none() && d.personal_tokens);
        assert_eq!(d.factors, vec![FactorKind::Totp, FactorKind::Webauthn]);
        assert_eq!(d.lockout_failures, 10);
        let s = ConsoleSettings::of(&json!({
            "auth": {"admin": {
                "local": {"enabled": true, "mfa": {"factors": ["totp"], "recovery_codes": 6},
                          "password_policy": {"min_length": 16, "max_age": "90d"},
                          "sso_required_domains": ["Acme.Example"]},
                "session": {"trusted_device": {"enabled": true, "max_age": "10d"}}
            }},
            "smtp": {"enabled": true, "host": "127.0.0.1", "port": 2525, "tls": "none", "from": "gw@acme.example"}
        }));
        assert!(s.local && s.allows(FactorKind::Totp) && !s.allows(FactorKind::Webauthn));
        assert_eq!(s.policy.min_length, 16);
        assert_eq!(s.password_max_age, Some(Duration::from_secs(90 * DAY)));
        assert_eq!(s.recovery_codes, 6);
        assert_eq!(s.sso_required_domains, vec!["acme.example"]);
        assert_eq!(s.trusted.unwrap().max_age, Duration::from_secs(10 * DAY));
        let mail = s.mail.unwrap();
        assert_eq!((mail.port, mail.security), (2525, antirion_mail::Security::None));
    }
}

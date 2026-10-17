//! Typed views of the console's operator-identity settings (`auth.admin` additions) and of `smtp`.

use serde::Deserialize;

fn yes() -> bool {
    true
}

/// The console additions of `auth.admin`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ConsoleAuthView {
    #[serde(default)]
    pub local: LocalView,
    #[serde(default)]
    pub invitations: InvitationsView,
    #[serde(default)]
    pub login_page: LoginPageView,
    #[serde(default)]
    pub personal_tokens: PersonalTokensView,
    #[serde(default)]
    pub session: ConsoleSessionView,
    #[serde(default)]
    pub login_throttle: ThrottleView,
    #[serde(default)]
    pub device_authorization: DeviceAuthorizationView,
}

/// `auth.admin.device_authorization`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct DeviceAuthorizationView {
    #[serde(default)]
    pub enabled: bool,
    pub code_ttl: Option<String>,
    pub poll_interval: Option<String>,
    pub token_ttl: Option<String>,
    pub max_starts_per_minute: Option<u32>,
}

/// `auth.admin.local`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct LocalView {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub password_policy: PasswordPolicyView,
    #[serde(default)]
    pub mfa: MfaView,
    #[serde(default)]
    pub sso_required_domains: Vec<String>,
    #[serde(default)]
    pub lockout: LockoutView,
    pub reset_token_ttl: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct PasswordPolicyView {
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub deny_common: Option<bool>,
    pub history: Option<usize>,
    /// A duration, or `none`.
    pub max_age: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct MfaView {
    pub factors: Option<Vec<String>>,
    #[serde(default)]
    pub webauthn: WebauthnView,
    pub recovery_codes: Option<usize>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct WebauthnView {
    pub rp_name: Option<String>,
    pub rp_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct LockoutView {
    pub max_failures: Option<u32>,
    pub window: Option<String>,
    pub duration: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct InvitationsView {
    pub ttl: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct LoginPageView {
    #[serde(default)]
    pub show_version: bool,
    #[serde(default)]
    pub ticker: TickerView,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TickerView {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PersonalTokensView {
    #[serde(default = "yes")]
    pub enabled: bool,
    pub max_lifetime: Option<String>,
}

impl Default for PersonalTokensView {
    fn default() -> PersonalTokensView {
        PersonalTokensView {
            enabled: true,
            max_lifetime: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ConsoleSessionView {
    #[serde(default)]
    pub trusted_device: TrustedDeviceView,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TrustedDeviceView {
    #[serde(default)]
    pub enabled: bool,
    pub max_age: Option<String>,
    pub idle_timeout: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ThrottleView {
    pub max_failures: Option<u32>,
    pub window: Option<String>,
}

/// `smtp`, with its password already resolved.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SmtpView {
    #[serde(default)]
    pub enabled: bool,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub tls: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: Option<String>,
    pub reply_to: Option<String>,
    pub timeout: Option<String>,
    pub max_messages_per_minute: Option<u32>,
}

//! Identity and access: the credential format and verifiers, data-plane credential selection, the
//! tenancy allow-lists and admin-plane roles.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::unreachable
)]

pub mod access;
pub mod certificate;
pub mod device;
pub mod enduser;
pub mod jwt;
pub mod keyfmt;
pub mod otp;
pub mod password;
pub mod permissions;
pub mod rbac;
pub mod roles;
pub mod saml;
pub mod select;
pub mod verifier;

/// Reasons of authentication and authorization failures.
pub mod reasons {
    antirion_error::reasons! {
        MISSING_API_KEY = "missing_api_key" => Unauthenticated,
        INVALID_API_KEY = "invalid_api_key" => Unauthenticated,
        EXPIRED_API_KEY = "expired_api_key" => Unauthenticated,
        INVALID_TOKEN = "invalid_token" => Unauthenticated,
        JWT_NO_MAPPING = "jwt_no_mapping" => Forbidden,
        JWKS_UNAVAILABLE = "jwks_unavailable" => DependencyUnavailable,
        AMBIGUOUS_CREDENTIALS = "ambiguous_credentials" => Unauthenticated,
        KEY_DISABLED = "key_disabled" => Forbidden,
        ORGANIZATION_DISABLED = "organization_disabled" => Forbidden,
        TEAM_DISABLED = "team_disabled" => Forbidden,
        PROJECT_DISABLED = "project_disabled" => Forbidden,
        USER_DISABLED = "user_disabled" => Forbidden,
        MODEL_NOT_ALLOWED = "model_not_allowed" => Forbidden,
        NO_ALLOWED_DEPLOYMENT = "no_allowed_deployment" => Forbidden,
        ENDPOINT_NOT_ALLOWED = "endpoint_not_allowed" => Forbidden,
        PERMISSION_REQUIRED = "permission_required" => Forbidden,
        OVERRIDE_NOT_ALLOWED = "override_not_allowed" => Forbidden,
        IP_NOT_ALLOWED = "ip_not_allowed" => Forbidden,
        RESERVED_TAG = "reserved_tag" => InvalidRequest,
        IDENTITY_STORE_UNAVAILABLE = "identity_store_unavailable" => DependencyUnavailable,
        AUTH_THROTTLED = "auth_throttled" => RateLimited,
        MISSING_END_USER = "missing_end_user" => InvalidRequest,
        ACCESS_RULE_DENIED = "access_rule_denied" => Forbidden,
        EXTERNAL_AUTHZ_DENIED = "external_authz_denied" => Forbidden,
        EXTERNAL_AUTHZ_UNAVAILABLE = "external_authz_unavailable" => DependencyUnavailable,
    }
}

pub use keyfmt::{Credential, Plane};
pub use permissions::{KeyPermission, KeyPermissions};
pub use verifier::Pepper;

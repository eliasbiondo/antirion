//! Validation errors: one entry of the Admin API's `details[]`.

use serde::Serialize;

use crate::DocPath;

/// The detail codes of a validation error.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetailCode {
    UnknownField,
    TypeMismatch,
    Required,
    OutOfRange,
    InvalidFormat,
    InvalidEnum,
    UnresolvedReference,
    DuplicateKey,
    Conflict,
    SecretUnresolved,
    SecretRequired,
    InvalidExpression,
    UnsupportedVersion,
    StaticSetting,
    LockedByBroaderLevel,
    ReservedName,
    NotSupportedInThisBuild,
    MalformedDocument,
    PasswordPolicy,
    LocalLoginDisabled,
}

impl DetailCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DetailCode::UnknownField => "unknown_field",
            DetailCode::TypeMismatch => "type_mismatch",
            DetailCode::Required => "required",
            DetailCode::OutOfRange => "out_of_range",
            DetailCode::InvalidFormat => "invalid_format",
            DetailCode::InvalidEnum => "invalid_enum",
            DetailCode::UnresolvedReference => "unresolved_reference",
            DetailCode::DuplicateKey => "duplicate_key",
            DetailCode::Conflict => "conflict",
            DetailCode::SecretUnresolved => "secret_unresolved",
            DetailCode::SecretRequired => "secret_required",
            DetailCode::InvalidExpression => "invalid_expression",
            DetailCode::UnsupportedVersion => "unsupported_version",
            DetailCode::StaticSetting => "static_setting",
            DetailCode::LockedByBroaderLevel => "locked_by_broader_level",
            DetailCode::ReservedName => "reserved_name",
            DetailCode::NotSupportedInThisBuild => "not_supported_in_this_build",
            DetailCode::MalformedDocument => "malformed_document",
            DetailCode::PasswordPolicy => "password_policy",
            DetailCode::LocalLoginDisabled => "local_login_disabled",
        }
    }
}

/// One problem found in a document, at its document path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConfigError {
    pub path: String,
    pub code: DetailCode,
    pub message: String,
}

impl ConfigError {
    #[must_use]
    pub fn new(path: &DocPath, code: DetailCode, message: impl Into<String>) -> ConfigError {
        ConfigError {
            path: path.to_string(),
            code,
            message: message.into(),
        }
    }
}

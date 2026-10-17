//! Local passwords: argon2id hashes in PHC form and the password policy. Hashing is deliberately
//! slow (tens of milliseconds), so callers run it off the async runtime.

use std::sync::OnceLock;

use argon2::password_hash::phc::PasswordHash;
use argon2::{Argon2, Params, PasswordHasher, PasswordVerifier};
use unicode_normalization::UnicodeNormalization;

/// A PHC hash nobody's password matches, verified for unknown accounts so that every failed sign-in
/// costs the same work.
const DUMMY_SALT: &[u8; 16] = b"antirion-dummy!!";

/// Why a hash could not be computed.
#[derive(Debug, thiserror::Error)]
pub enum HashError {
    #[error("no random source")]
    Random,
    #[error("argon2: {0}")]
    Argon2(String),
}

fn normalized(password: &str) -> String {
    password.nfkc().collect()
}

/// The argon2id hash of a password (NFKC-normalised) with a fresh 128-bit salt, in PHC form. The
/// parameters are at least m = 19 MiB, t = 2, p = 1.
pub fn hash(password: &str) -> Result<String, HashError> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|_| HashError::Random)?;
    hash_with_salt(password, &salt)
}

fn hash_with_salt(password: &str, salt: &[u8]) -> Result<String, HashError> {
    Argon2::default()
        .hash_password_with_salt(normalized(password).as_bytes(), salt)
        .map(|h| h.to_string())
        .map_err(|e| HashError::Argon2(e.to_string()))
}

/// Whether `password` matches the PHC hash `stored`.
#[must_use]
pub fn verify(password: &str, stored: &str) -> bool {
    Argon2::default()
        .verify_password(normalized(password).as_bytes(), stored)
        .is_ok()
}

/// Does the work of one verification and fails: what a sign-in with an unknown email costs.
pub fn verify_nothing(password: &str) {
    static DUMMY: OnceLock<String> = OnceLock::new();
    let dummy = DUMMY.get_or_init(|| hash_with_salt("", DUMMY_SALT).unwrap_or_default());
    let _ = verify(&format!("{password}\u{0}"), dummy);
}

/// Whether a stored hash uses weaker parameters than the current ones, so it is replaced at the
/// next successful sign-in.
#[must_use]
pub fn needs_rehash(stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return true;
    };
    let Ok(params) = Params::try_from(&parsed) else {
        return true;
    };
    let current = Params::default();
    parsed.algorithm.as_str() != "argon2id"
        || params.m_cost() < current.m_cost()
        || params.t_cost() < current.t_cost()
        || params.p_cost() < current.p_cost()
}

/// `auth.admin.local.password_policy`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub min_length: usize,
    pub max_length: usize,
    pub deny_common: bool,
    pub history: usize,
}

impl Default for Policy {
    fn default() -> Policy {
        Policy {
            min_length: 12,
            max_length: 128,
            deny_common: true,
            history: 5,
        }
    }
}

/// The rule a new password failed, named as the policy field it breaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    MinLength,
    MaxLength,
    Common,
    Email,
    History,
}

impl Rule {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::MinLength => "min_length",
            Rule::MaxLength => "max_length",
            Rule::Common => "deny_common",
            Rule::Email => "email",
            Rule::History => "history",
        }
    }

    /// A sentence for the error detail.
    #[must_use]
    pub fn message(self, policy: &Policy) -> String {
        match self {
            Rule::MinLength => format!("the password needs at least {} characters", policy.min_length),
            Rule::MaxLength => format!("the password may have at most {} characters", policy.max_length),
            Rule::Common => "the password is too common".to_owned(),
            Rule::Email => "the password may not be the email address or its local part".to_owned(),
            Rule::History => format!(
                "the password may not repeat one of the last {} passwords",
                policy.history
            ),
        }
    }
}

fn common_list() -> &'static std::collections::HashSet<&'static str> {
    static LIST: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new();
    LIST.get_or_init(|| {
        include_str!("../data/common-passwords.txt")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect()
    })
}

/// Whether a password is a listed common password, also once the digits and symbols people add
/// around a common word (`password123456`, `!qwerty1`) are taken away.
#[must_use]
pub fn is_common(password: &str) -> bool {
    let lower = normalized(password).to_lowercase();
    let list = common_list();
    if list.contains(lower.as_str()) {
        return true;
    }
    let core = lower.trim_matches(|c: char| !c.is_alphabetic());
    if !core.is_empty() && list.contains(core) {
        return true;
    }
    let mut chars = lower.chars();
    let first = chars.next();
    first.is_some_and(|f| chars.all(|c| c == f))
}

/// Checks a new password against the policy, except the history rule (see [`repeats`]).
pub fn check(policy: &Policy, password: &str, email: Option<&str>) -> Result<(), Rule> {
    let length = normalized(password).chars().count();
    if length < policy.min_length {
        return Err(Rule::MinLength);
    }
    if length > policy.max_length {
        return Err(Rule::MaxLength);
    }
    if policy.deny_common && is_common(password) {
        return Err(Rule::Common);
    }
    if let Some(email) = email {
        let lower = normalized(password).to_lowercase();
        let email = email.to_lowercase();
        let local = email.split('@').next().unwrap_or_default();
        if lower == email || (!local.is_empty() && lower == local) {
            return Err(Rule::Email);
        }
    }
    Ok(())
}

/// Whether `password` equals one of the `history` most recent hashes (newest first).
#[must_use]
pub fn repeats(policy: &Policy, password: &str, previous: &[String]) -> bool {
    previous.iter().take(policy.history).any(|h| verify(password, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_argon2id_and_verify() {
        let h = hash("correct horse battery").unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{h}");
        assert!(verify("correct horse battery", &h));
        assert!(!verify("correct horse batterz", &h));
        assert!(!needs_rehash(&h));
        assert_ne!(
            h,
            hash("correct horse battery").unwrap(),
            "every hash has its own salt"
        );
        let weak = Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            Params::new(8 * 1024, 1, 1, None).unwrap(),
        )
        .hash_password_with_salt(b"x", b"0123456789abcdef")
        .unwrap()
        .to_string();
        assert!(needs_rehash(&weak));
        verify_nothing("anything");
    }

    #[test]
    fn normalisation_makes_equivalent_spellings_equal() {
        let h = hash("caf\u{e9} au lait!").unwrap();
        assert!(verify("cafe\u{301} au lait!", &h));
    }

    #[test]
    fn policy_rules() {
        let p = Policy::default();
        assert_eq!(check(&p, "short-pass1", None), Err(Rule::MinLength));
        assert_eq!(check(&p, &"x".repeat(129), None).unwrap_err(), Rule::MaxLength);
        assert_eq!(check(&p, "password123456", None), Err(Rule::Common));
        assert_eq!(check(&p, "!!Qwertyuiop2024", None), Err(Rule::Common));
        assert_eq!(check(&p, "aaaaaaaaaaaaaaa", None), Err(Rule::Common));
        assert_eq!(
            check(&p, "alice.martins", Some("Alice.Martins@acme.example")),
            Err(Rule::Email)
        );
        assert!(check(&p, "tangerine-otter-42-violin", Some("a@b.c")).is_ok());
        let permissive = Policy {
            deny_common: false,
            ..Policy::default()
        };
        assert!(check(&permissive, "password123456", None).is_ok());
    }

    #[test]
    fn history_compares_with_the_newest_hashes_only() {
        let p = Policy {
            history: 1,
            ..Policy::default()
        };
        let old = vec![
            hash("first-password-1").unwrap(),
            hash("older-password-2").unwrap(),
        ];
        assert!(repeats(&p, "first-password-1", &old));
        assert!(!repeats(&p, "older-password-2", &old));
    }
}

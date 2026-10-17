//! SAML 2.0 web browser single sign-on, service provider side: the identity provider's metadata
//! and certificates, the HTTP-Redirect `AuthnRequest`, the service provider's metadata, and the
//! check of an HTTP-POST response (XML signature with exclusive canonicalization, audience,
//! recipient, time window and request binding). Replay protection and sessions belong to the
//! caller, which keeps each accepted assertion id until [`Assertion::valid_until`].

mod c14n;
mod cert;
mod config;
mod dsig;
mod metadata;
mod request;
mod response;
mod xml;

pub use self::cert::{Certificate, from_pem};
pub use self::config::{
    IdpSource, Provider, check_document, check_transport, compile, is_plaintext, name_id_format,
};
pub use self::metadata::{IdpDescriptor, parse_idp_metadata, sp_metadata};
pub use self::request::AuthnRequest;
pub use self::response::{Assertion, Expected, validate};

/// The `InResponseTo` a response claims, read before anything is verified: it only finds the
/// request to check the response against.
pub fn in_response_to(xml: &str) -> Result<Option<String>, SamlError> {
    let doc = xml::parse(xml)?;
    let root = doc.root_element();
    if !xml::is(&root, xml::PROTOCOL, "Response") {
        return Err(SamlError::Malformed);
    }
    Ok(root.attribute("InResponseTo").map(str::to_owned))
}

/// The most bytes a decoded response may have.
pub const MAX_RESPONSE_BYTES: usize = xml::MAX_BYTES;

/// Why a SAML message or metadata document was refused. The text is fixed: nothing of the message
/// is echoed except a status code's last word.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SamlError {
    #[error("the SAML message is malformed")]
    Malformed,
    #[error("the assertion is not signed")]
    Unsigned,
    #[error("the signature does not verify with a trusted certificate")]
    BadSignature,
    #[error("the message uses a signature or digest algorithm this gateway does not accept")]
    UnsupportedAlgorithm,
    #[error("the identity provider refused the sign-in ({})", .0.as_deref().unwrap_or("no status"))]
    Status(Option<String>),
    #[error("the response or assertion comes from another issuer")]
    Issuer,
    #[error("the response is addressed to another destination")]
    Destination,
    #[error("the assertion is meant for another audience")]
    Audience,
    #[error("the assertion is meant for another recipient")]
    Recipient,
    #[error("the assertion has expired")]
    Expired,
    #[error("the assertion is not valid yet")]
    NotYetValid,
    #[error("the response does not answer the sign-in this gateway started")]
    InResponseTo,
    #[error("unsolicited responses are not accepted")]
    Unsolicited,
    #[error("encrypted assertions are not supported")]
    Encrypted,
    #[error("the assertion was used already")]
    Replayed,
    #[error("a certificate cannot be read")]
    Certificate,
    #[error("the metadata cannot be used: {0}")]
    Metadata(&'static str),
}

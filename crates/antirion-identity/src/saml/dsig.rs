//! XML Signature over one element, the only profile SAML responses need: an enveloped signature
//! that is a child of the element it signs, one reference to that element's `ID`, the
//! enveloped-signature and exclusive canonicalization transforms, SHA-2 digests, and RSA (PKCS#1
//! v1.5) or ECDSA signatures checked against the configured certificates only. Key material in the
//! message is never trusted.

use aws_lc_rs::signature::{
    ECDSA_P256_SHA256_FIXED, ECDSA_P384_SHA384_FIXED, RSA_PKCS1_2048_8192_SHA256, RSA_PKCS1_2048_8192_SHA384,
    RSA_PKCS1_2048_8192_SHA512, UnparsedPublicKey, VerificationAlgorithm,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use roxmltree::{Document, Node};
use sha2::Digest;
use subtle::ConstantTimeEq;

use super::SamlError;
use super::c14n::{Options, canonicalize};
use super::cert::Certificate;
use super::xml::{DSIG, EXC_C14N_NS, children, only};

const EXC_C14N: &str = "http://www.w3.org/2001/10/xml-exc-c14n#";
const EXC_C14N_COMMENTS: &str = "http://www.w3.org/2001/10/xml-exc-c14n#WithComments";
const ENVELOPED: &str = "http://www.w3.org/2000/09/xmldsig#enveloped-signature";

fn algorithm(uri: &str) -> Option<&'static dyn VerificationAlgorithm> {
    Some(match uri {
        "http://www.w3.org/2001/04/xmldsig-more#rsa-sha256" => &RSA_PKCS1_2048_8192_SHA256,
        "http://www.w3.org/2001/04/xmldsig-more#rsa-sha384" => &RSA_PKCS1_2048_8192_SHA384,
        "http://www.w3.org/2001/04/xmldsig-more#rsa-sha512" => &RSA_PKCS1_2048_8192_SHA512,
        "http://www.w3.org/2001/04/xmldsig-more#ecdsa-sha256" => &ECDSA_P256_SHA256_FIXED,
        "http://www.w3.org/2001/04/xmldsig-more#ecdsa-sha384" => &ECDSA_P384_SHA384_FIXED,
        _ => return None,
    })
}

fn digest(uri: &str, data: &[u8]) -> Option<Vec<u8>> {
    Some(match uri {
        "http://www.w3.org/2001/04/xmlenc#sha256" => sha2::Sha256::digest(data).to_vec(),
        "http://www.w3.org/2001/04/xmldsig-more#sha384" => sha2::Sha384::digest(data).to_vec(),
        "http://www.w3.org/2001/04/xmlenc#sha512" => sha2::Sha512::digest(data).to_vec(),
        _ => return None,
    })
}

fn decode(text: &str) -> Result<Vec<u8>, SamlError> {
    let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    STANDARD.decode(compact).map_err(|_| SamlError::BadSignature)
}

fn required<'a, 'i>(n: Node<'a, 'i>, local: &'a str) -> Result<Node<'a, 'i>, SamlError> {
    only(n, DSIG, local)?.ok_or(SamlError::BadSignature)
}

/// The canonicalization a `CanonicalizationMethod` or `Transform` element names, with its
/// `InclusiveNamespaces` prefixes.
fn c14n_of(method: Node<'_, '_>) -> Result<Options, SamlError> {
    let comments = match method.attribute("Algorithm") {
        Some(EXC_C14N) => false,
        Some(EXC_C14N_COMMENTS) => true,
        _ => return Err(SamlError::UnsupportedAlgorithm),
    };
    let inclusive = only(method, EXC_C14N_NS, "InclusiveNamespaces")?
        .and_then(|n| n.attribute("PrefixList"))
        .map(|l| l.split_ascii_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    Ok(Options {
        comments,
        inclusive,
        exclude: None,
    })
}

/// Whether `element` carries a signature of its own.
pub(crate) fn is_signed(element: Node<'_, '_>) -> bool {
    children(element, DSIG, "Signature").next().is_some()
}

/// Verifies the enveloped signature of `element` with one of `trusted`.
pub(crate) fn verify(
    doc: &Document<'_>,
    element: Node<'_, '_>,
    trusted: &[Certificate],
) -> Result<(), SamlError> {
    let signature = only(element, DSIG, "Signature")?.ok_or(SamlError::Unsigned)?;
    let id = element
        .attribute("ID")
        .filter(|v| !v.is_empty())
        .ok_or(SamlError::BadSignature)?;
    let same_id = doc
        .descendants()
        .filter(|n| n.is_element() && n.attribute("ID") == Some(id))
        .count();
    if same_id != 1 {
        return Err(SamlError::BadSignature);
    }
    let signed_info = required(signature, "SignedInfo")?;
    let canonical = c14n_of(required(signed_info, "CanonicalizationMethod")?)?;
    let alg = required(signed_info, "SignatureMethod")?
        .attribute("Algorithm")
        .and_then(algorithm)
        .ok_or(SamlError::UnsupportedAlgorithm)?;
    let reference = required(signed_info, "Reference")?;
    if reference.attribute("URI") != Some(&format!("#{id}")) {
        return Err(SamlError::BadSignature);
    }
    let transforms: Vec<Node<'_, '_>> =
        children(required(reference, "Transforms")?, DSIG, "Transform").collect();
    let [enveloped, exclusive] = transforms.as_slice() else {
        return Err(SamlError::UnsupportedAlgorithm);
    };
    if enveloped.attribute("Algorithm") != Some(ENVELOPED) {
        return Err(SamlError::UnsupportedAlgorithm);
    }
    let mut content = c14n_of(*exclusive)?;
    content.exclude = Some(signature.id());
    let digest_uri = required(reference, "DigestMethod")?
        .attribute("Algorithm")
        .ok_or(SamlError::UnsupportedAlgorithm)?;
    let expected = decode(&super::xml::text(required(reference, "DigestValue")?)?)?;
    let actual = digest(digest_uri, canonicalize(doc, element, &content).as_bytes())
        .ok_or(SamlError::UnsupportedAlgorithm)?;
    if expected.len() != actual.len() || !bool::from(expected.ct_eq(&actual)) {
        return Err(SamlError::BadSignature);
    }
    let value = decode(&super::xml::text(required(signature, "SignatureValue")?)?)?;
    let signed = canonicalize(doc, signed_info, &canonical);
    let verified = trusted.iter().any(|c| {
        UnparsedPublicKey::new(alg, &c.spki)
            .verify(signed.as_bytes(), &value)
            .is_ok()
    });
    if verified {
        Ok(())
    } else {
        Err(SamlError::BadSignature)
    }
}

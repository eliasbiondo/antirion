//! SAML metadata: the identity provider's (its entity id, its HTTP-Redirect single sign-on
//! endpoint and its signing certificates) read from its metadata document, and the service
//! provider's own document for the identity provider to import.

use super::SamlError;
use super::cert::{Certificate, from_base64};
use super::xml::{DSIG, METADATA, children, only, parse_metadata, text};

const REDIRECT: &str = "urn:oasis:names:tc:SAML:2.0:bindings:HTTP-Redirect";
const POST: &str = "urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST";
const SAML2: &str = "urn:oasis:names:tc:SAML:2.0:protocol";

/// What the service provider needs of an identity provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdpDescriptor {
    pub entity_id: String,
    /// The HTTP-Redirect `SingleSignOnService` location.
    pub sso_url: String,
    pub certificates: Vec<Certificate>,
}

/// Reads an identity provider's metadata: an `EntityDescriptor`, or an `EntitiesDescriptor` that
/// holds exactly one entity with an `IDPSSODescriptor` for SAML 2.0.
pub fn parse_idp_metadata(xml: &str) -> Result<IdpDescriptor, SamlError> {
    let doc = parse_metadata(xml).map_err(|_| SamlError::Metadata("the document is not well-formed XML"))?;
    let root = doc.root_element();
    let entities: Vec<_> = if super::xml::is(&root, METADATA, "EntityDescriptor") {
        vec![root]
    } else if super::xml::is(&root, METADATA, "EntitiesDescriptor") {
        root.descendants()
            .filter(|n| super::xml::is(n, METADATA, "EntityDescriptor"))
            .collect()
    } else {
        return Err(SamlError::Metadata("the document is not SAML metadata"));
    };
    let mut found = Vec::new();
    for entity in entities {
        for idp in children(entity, METADATA, "IDPSSODescriptor") {
            let protocols = idp.attribute("protocolSupportEnumeration").unwrap_or_default();
            if protocols.split_ascii_whitespace().any(|p| p == SAML2) {
                found.push((entity, idp));
            }
        }
    }
    let [(entity, idp)] = found.as_slice() else {
        return Err(SamlError::Metadata(
            "the metadata must describe exactly one SAML 2.0 identity provider",
        ));
    };
    let entity_id = entity
        .attribute("entityID")
        .filter(|e| !e.is_empty())
        .ok_or(SamlError::Metadata("the identity provider has no entityID"))?;
    let sso_url = children(*idp, METADATA, "SingleSignOnService")
        .find(|s| s.attribute("Binding") == Some(REDIRECT))
        .and_then(|s| s.attribute("Location"))
        .filter(|l| !l.is_empty())
        .ok_or(SamlError::Metadata(
            "the identity provider has no HTTP-Redirect single sign-on service",
        ))?;
    let mut certificates = Vec::new();
    for key in children(*idp, METADATA, "KeyDescriptor") {
        if key.attribute("use").is_some_and(|u| u != "signing") {
            continue;
        }
        let Some(info) = only(key, DSIG, "KeyInfo")? else {
            continue;
        };
        for data in children(info, DSIG, "X509Data") {
            for c in children(data, DSIG, "X509Certificate") {
                certificates.push(from_base64(&text(c)?)?);
            }
        }
    }
    if certificates.is_empty() {
        return Err(SamlError::Metadata(
            "the identity provider has no signing certificate",
        ));
    }
    Ok(IdpDescriptor {
        entity_id: entity_id.to_owned(),
        sso_url: sso_url.to_owned(),
        certificates,
    })
}

/// Escapes text for an XML attribute or element.
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The service provider's metadata: it wants signed assertions, does not sign its requests, and
/// consumes responses with HTTP-POST at `acs_url`.
#[must_use]
pub fn sp_metadata(entity_id: &str, acs_url: &str, name_id_format: &str) -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<md:EntityDescriptor xmlns:md="{md}" entityID="{entity}">"#,
            r#"<md:SPSSODescriptor AuthnRequestsSigned="false" WantAssertionsSigned="true" protocolSupportEnumeration="{saml2}">"#,
            r#"<md:NameIDFormat>{format}</md:NameIDFormat>"#,
            r#"<md:AssertionConsumerService Binding="{post}" Location="{acs}" index="0" isDefault="true"/>"#,
            r#"</md:SPSSODescriptor></md:EntityDescriptor>"#
        ),
        md = METADATA,
        entity = escape(entity_id),
        saml2 = SAML2,
        format = escape(name_id_format),
        post = POST,
        acs = escape(acs_url),
    )
}

#[cfg(test)]
mod tests {
    use super::super::cert::tests::PEM;
    use super::*;

    fn body(pem: &str) -> String {
        pem.lines().filter(|l| !l.starts_with("-----")).collect()
    }

    pub(crate) fn idp_metadata(cert_b64: &str) -> String {
        format!(
            r#"<md:EntityDescriptor xmlns:md="urn:oasis:names:tc:SAML:2.0:metadata" xmlns:ds="http://www.w3.org/2000/09/xmldsig#" entityID="https://idp.test.example/metadata">
  <md:IDPSSODescriptor protocolSupportEnumeration="urn:oasis:names:tc:SAML:2.0:protocol">
    <md:KeyDescriptor use="signing"><ds:KeyInfo><ds:X509Data><ds:X509Certificate>
      {cert_b64}
    </ds:X509Certificate></ds:X509Data></ds:KeyInfo></md:KeyDescriptor>
    <md:KeyDescriptor use="encryption"><ds:KeyInfo><ds:X509Data><ds:X509Certificate>AAAA</ds:X509Certificate></ds:X509Data></ds:KeyInfo></md:KeyDescriptor>
    <md:SingleSignOnService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST" Location="https://idp.test.example/post"/>
    <md:SingleSignOnService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-Redirect" Location="https://idp.test.example/sso"/>
  </md:IDPSSODescriptor>
</md:EntityDescriptor>"#
        )
    }

    #[test]
    fn idp_metadata_gives_the_redirect_endpoint_and_signing_keys() {
        let d = parse_idp_metadata(&idp_metadata(&body(PEM))).unwrap();
        assert_eq!(d.entity_id, "https://idp.test.example/metadata");
        assert_eq!(d.sso_url, "https://idp.test.example/sso");
        assert_eq!(d.certificates.len(), 1);
        let wrapped = format!(
            r#"<md:EntitiesDescriptor xmlns:md="urn:oasis:names:tc:SAML:2.0:metadata">{}</md:EntitiesDescriptor>"#,
            idp_metadata(&body(PEM))
        );
        assert_eq!(parse_idp_metadata(&wrapped).unwrap(), d);
    }

    #[test]
    fn incomplete_metadata_is_refused() {
        let no_redirect = idp_metadata(&body(PEM)).replace("HTTP-Redirect", "SOAP");
        assert!(matches!(
            parse_idp_metadata(&no_redirect),
            Err(SamlError::Metadata(_))
        ));
        assert!(matches!(parse_idp_metadata("<a/>"), Err(SamlError::Metadata(_))));
        let no_key = idp_metadata(&body(PEM)).replace(r#"use="signing""#, r#"use="encryption""#);
        assert!(parse_idp_metadata(&no_key).is_err());
    }

    #[test]
    fn sp_metadata_escapes_its_values() {
        let xml = sp_metadata("https://gw.example/a?x=1&y=2", "https://gw.example/acs", "urn:f");
        assert!(xml.contains(r#"entityID="https://gw.example/a?x=1&amp;y=2""#));
        assert!(xml.contains(r#"WantAssertionsSigned="true""#));
        assert!(super::super::xml::parse(&xml).is_ok());
    }
}

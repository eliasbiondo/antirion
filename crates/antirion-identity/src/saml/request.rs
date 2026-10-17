//! The `AuthnRequest` a service-provider-initiated sign-in sends with the HTTP-Redirect binding:
//! unsigned, asking for an HTTP-POST response at the assertion consumer service, DEFLATE-compressed,
//! base64-encoded and URL-encoded into the identity provider's single sign-on URL.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::metadata::escape;
use super::xml::{ASSERTION, PROTOCOL};

const POST: &str = "urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST";

/// One sign-in request.
#[derive(Clone, Debug)]
pub struct AuthnRequest<'a> {
    /// Starts with a letter or `_` (an XML NCName).
    pub id: &'a str,
    /// `YYYY-MM-DDTHH:MM:SSZ`.
    pub issue_instant: &'a str,
    pub destination: &'a str,
    pub sp_entity_id: &'a str,
    pub acs_url: &'a str,
    pub name_id_format: &'a str,
}

impl AuthnRequest<'_> {
    /// The request's XML.
    #[must_use]
    pub fn xml(&self) -> String {
        format!(
            concat!(
                r#"<samlp:AuthnRequest xmlns:samlp="{p}" xmlns:saml="{a}" ID="{id}" Version="2.0" "#,
                r#"IssueInstant="{at}" Destination="{dest}" ProtocolBinding="{post}" "#,
                r#"AssertionConsumerServiceURL="{acs}">"#,
                r#"<saml:Issuer>{issuer}</saml:Issuer>"#,
                r#"<samlp:NameIDPolicy Format="{format}" AllowCreate="true"/>"#,
                r#"</samlp:AuthnRequest>"#
            ),
            p = PROTOCOL,
            a = ASSERTION,
            id = escape(self.id),
            at = escape(self.issue_instant),
            dest = escape(self.destination),
            post = POST,
            acs = escape(self.acs_url),
            issuer = escape(self.sp_entity_id),
            format = escape(self.name_id_format),
        )
    }

    /// The URL the browser is sent to: the destination with `SAMLRequest` (and `RelayState`)
    /// added to its query.
    #[must_use]
    pub fn redirect_url(&self, relay_state: Option<&str>) -> String {
        let deflated = miniz_oxide::deflate::compress_to_vec(self.xml().as_bytes(), 6);
        let mut url = self.destination.to_owned();
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str("SAMLRequest=");
        url.push_str(&url_encode(&STANDARD.encode(deflated)));
        if let Some(r) = relay_state {
            url.push_str("&RelayState=");
            url.push_str(&url_encode(r));
        }
        url
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(char::from(b)),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> AuthnRequest<'static> {
        AuthnRequest {
            id: "_0123abcd",
            issue_instant: "2026-10-15T12:00:00Z",
            destination: "https://idp.test.example/sso?tenant=a",
            sp_entity_id: "https://gw.example/admin/v1/auth/saml/corp/metadata",
            acs_url: "https://gw.example/admin/v1/auth/saml/corp/acs",
            name_id_format: "urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress",
        }
    }

    #[test]
    fn the_redirect_carries_the_deflated_request() {
        let url = request().redirect_url(Some("r/1"));
        assert!(url.starts_with("https://idp.test.example/sso?tenant=a&SAMLRequest="));
        assert!(url.ends_with("&RelayState=r%2F1"));
        let encoded = url
            .split("SAMLRequest=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap();
        let mut b64 = String::new();
        let mut rest = encoded;
        while let Some(i) = rest.find('%') {
            b64.push_str(&rest[..i]);
            b64.push(char::from(u8::from_str_radix(&rest[i + 1..i + 3], 16).unwrap()));
            rest = &rest[i + 3..];
        }
        b64.push_str(rest);
        let raw = STANDARD.decode(b64).unwrap();
        let xml = miniz_oxide::inflate::decompress_to_vec(&raw).unwrap();
        let xml = String::from_utf8(xml).unwrap();
        assert_eq!(xml, request().xml());
        let doc = super::super::xml::parse(&xml).unwrap();
        let root = doc.root_element();
        assert_eq!(root.attribute("ID"), Some("_0123abcd"));
        assert_eq!(
            root.attribute("AssertionConsumerServiceURL"),
            Some("https://gw.example/admin/v1/auth/saml/corp/acs")
        );
    }
}

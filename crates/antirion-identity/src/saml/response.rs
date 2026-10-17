//! The service provider's check of a `samlp:Response` posted to its assertion consumer service:
//! the response is for this SP and, unless unsolicited responses are allowed, for the request it
//! sent; it holds exactly one assertion, signed by the identity provider (and the response's own
//! signature, when it has one, verifies too); the assertion's issuer, bearer subject confirmation,
//! conditions and authentication statement hold now. Everything returned is read from the verified
//! assertion only, never from elsewhere in the message.

use roxmltree::{Document, Node};

use super::SamlError;
use super::cert::Certificate;
use super::dsig::{is_signed, verify};
use super::xml::{ASSERTION, PROTOCOL, children, only, parse, text};

const SUCCESS: &str = "urn:oasis:names:tc:SAML:2.0:status:Success";
const BEARER: &str = "urn:oasis:names:tc:SAML:2.0:cm:bearer";

/// What the response must match.
#[derive(Clone, Debug)]
pub struct Expected<'a> {
    pub idp_entity_id: &'a str,
    pub sp_entity_id: &'a str,
    pub acs_url: &'a str,
    /// The id of the request this response answers; `None` for an unsolicited response, which then
    /// must not name one.
    pub request_id: Option<&'a str>,
    pub certificates: &'a [Certificate],
    /// Unix seconds.
    pub now: i64,
    /// Allowed clock difference, seconds.
    pub skew: i64,
}

/// A verified assertion's facts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Assertion {
    pub id: String,
    pub name_id: Option<String>,
    pub name_id_format: Option<String>,
    pub session_index: Option<String>,
    pub authn_context: Option<String>,
    /// Attribute name and its values, in document order.
    pub attributes: Vec<(String, Vec<String>)>,
    /// Until when the assertion could still be accepted (Unix seconds): how long its id is kept
    /// against replay.
    pub valid_until: i64,
}

impl Assertion {
    /// The values of attribute `name`.
    #[must_use]
    pub fn values(&self, name: &str) -> Vec<&str> {
        self.attributes
            .iter()
            .filter(|(n, _)| n == name)
            .flat_map(|(_, v)| v.iter().map(String::as_str))
            .collect()
    }
}

/// `xs:dateTime` in UTC as Unix seconds.
fn instant(v: &str) -> Result<i64, SamlError> {
    antirion_core::UtcMicros::parse_rfc3339(v)
        .map(|t| t.0.div_euclid(1_000_000))
        .map_err(|_| SamlError::Malformed)
}

fn attribute_instant(n: Node<'_, '_>, name: &str) -> Result<Option<i64>, SamlError> {
    n.attribute(name).map(instant).transpose()
}

fn issuer(n: Node<'_, '_>) -> Result<Option<String>, SamlError> {
    only(n, ASSERTION, "Issuer")?.map(text).transpose()
}

/// The status code's last segment, when it is a short plain word, for the refusal's message.
fn status_word(code: &str) -> Option<String> {
    let word = code.rsplit(':').next()?;
    (word.len() <= 40 && word.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| word.to_owned())
}

/// The bearer confirmation of `subject` that holds; its `NotOnOrAfter`.
fn confirmation(subject: Node<'_, '_>, e: &Expected<'_>) -> Result<i64, SamlError> {
    let mut last = SamlError::Recipient;
    for c in children(subject, ASSERTION, "SubjectConfirmation") {
        if c.attribute("Method") != Some(BEARER) {
            continue;
        }
        let Some(data) = only(c, ASSERTION, "SubjectConfirmationData")? else {
            continue;
        };
        if data.attribute("Recipient") != Some(e.acs_url) {
            last = SamlError::Recipient;
            continue;
        }
        if data.attribute("InResponseTo") != e.request_id {
            last = SamlError::InResponseTo;
            continue;
        }
        let Some(until) = attribute_instant(data, "NotOnOrAfter")? else {
            last = SamlError::Expired;
            continue;
        };
        if e.now - e.skew >= until {
            last = SamlError::Expired;
            continue;
        }
        if attribute_instant(data, "NotBefore")?.is_some_and(|nb| e.now + e.skew < nb) {
            last = SamlError::NotYetValid;
            continue;
        }
        return Ok(until);
    }
    Err(last)
}

/// The conditions' `NotOnOrAfter`, after checking the window and the audiences.
fn conditions(assertion: Node<'_, '_>, e: &Expected<'_>) -> Result<Option<i64>, SamlError> {
    let c = only(assertion, ASSERTION, "Conditions")?.ok_or(SamlError::Audience)?;
    if attribute_instant(c, "NotBefore")?.is_some_and(|nb| e.now + e.skew < nb) {
        return Err(SamlError::NotYetValid);
    }
    let until = attribute_instant(c, "NotOnOrAfter")?;
    if until.is_some_and(|u| e.now - e.skew >= u) {
        return Err(SamlError::Expired);
    }
    let mut restricted = false;
    for r in children(c, ASSERTION, "AudienceRestriction") {
        restricted = true;
        let mut ours = false;
        for a in children(r, ASSERTION, "Audience") {
            ours |= text(a)? == e.sp_entity_id;
        }
        if !ours {
            return Err(SamlError::Audience);
        }
    }
    if !restricted {
        return Err(SamlError::Audience);
    }
    Ok(until)
}

fn attributes(assertion: Node<'_, '_>) -> Result<Vec<(String, Vec<String>)>, SamlError> {
    let mut out = Vec::new();
    for statement in children(assertion, ASSERTION, "AttributeStatement") {
        for a in children(statement, ASSERTION, "Attribute") {
            let name = a.attribute("Name").ok_or(SamlError::Malformed)?.to_owned();
            let values = children(a, ASSERTION, "AttributeValue")
                .map(text)
                .collect::<Result<Vec<_>, _>>()?;
            out.push((name, values));
        }
    }
    Ok(out)
}

fn check_assertion(
    doc: &Document<'_>,
    assertion: Node<'_, '_>,
    e: &Expected<'_>,
) -> Result<Assertion, SamlError> {
    if !is_signed(assertion) {
        return Err(SamlError::Unsigned);
    }
    verify(doc, assertion, e.certificates)?;
    if assertion.attribute("Version") != Some("2.0") {
        return Err(SamlError::Malformed);
    }
    if issuer(assertion)?.as_deref() != Some(e.idp_entity_id) {
        return Err(SamlError::Issuer);
    }
    let subject = only(assertion, ASSERTION, "Subject")?.ok_or(SamlError::Malformed)?;
    let confirmed_until = confirmation(subject, e)?;
    let conditions_until = conditions(assertion, e)?;
    let authn = only(assertion, ASSERTION, "AuthnStatement")?.ok_or(SamlError::Malformed)?;
    if attribute_instant(authn, "SessionNotOnOrAfter")?.is_some_and(|s| e.now - e.skew >= s) {
        return Err(SamlError::Expired);
    }
    let authn_context = match only(authn, ASSERTION, "AuthnContext")? {
        Some(ctx) => only(ctx, ASSERTION, "AuthnContextClassRef")?
            .map(text)
            .transpose()?,
        None => None,
    };
    let name_id = only(subject, ASSERTION, "NameID")?;
    Ok(Assertion {
        id: assertion.attribute("ID").unwrap_or_default().to_owned(),
        name_id: name_id.map(text).transpose()?,
        name_id_format: name_id.and_then(|n| n.attribute("Format")).map(str::to_owned),
        session_index: authn.attribute("SessionIndex").map(str::to_owned),
        authn_context,
        attributes: attributes(assertion)?,
        valid_until: conditions_until.map_or(confirmed_until, |c| c.max(confirmed_until)) + e.skew,
    })
}

/// Checks the XML of a response and returns its assertion.
pub fn validate(xml: &str, e: &Expected<'_>) -> Result<Assertion, SamlError> {
    let doc = parse(xml)?;
    let response = doc.root_element();
    if !(response.tag_name().namespace() == Some(PROTOCOL) && response.tag_name().name() == "Response") {
        return Err(SamlError::Malformed);
    }
    if response.attribute("Version") != Some("2.0") {
        return Err(SamlError::Malformed);
    }
    if response.attribute("Destination").is_some_and(|d| d != e.acs_url) {
        return Err(SamlError::Destination);
    }
    if response.attribute("InResponseTo") != e.request_id {
        return Err(if e.request_id.is_some() {
            SamlError::InResponseTo
        } else {
            SamlError::Unsolicited
        });
    }
    if issuer(response)?.is_some_and(|i| i != e.idp_entity_id) {
        return Err(SamlError::Issuer);
    }
    let status = only(response, PROTOCOL, "Status")?.ok_or(SamlError::Malformed)?;
    let code = only(status, PROTOCOL, "StatusCode")?
        .and_then(|c| c.attribute("Value"))
        .ok_or(SamlError::Malformed)?;
    if code != SUCCESS {
        return Err(SamlError::Status(status_word(code)));
    }
    if is_signed(response) {
        verify(&doc, response, e.certificates)?;
    }
    if children(response, ASSERTION, "EncryptedAssertion")
        .next()
        .is_some()
    {
        return Err(SamlError::Encrypted);
    }
    let assertion = only(response, ASSERTION, "Assertion")?.ok_or(SamlError::Malformed)?;
    check_assertion(&doc, assertion, e)
}

#[cfg(test)]
mod tests {
    use super::super::cert::from_pem;
    use super::*;

    const RSA: &str = include_str!("testdata/response-rsa.xml");
    const EC: &str = include_str!("testdata/response-ec.xml");
    const SHA512_COMMENTS: &str = include_str!("testdata/response-rsa-sha512-comments.xml");
    const BOTH: &str = include_str!("testdata/response-both-signed.xml");
    const UNSOLICITED: &str = include_str!("testdata/response-unsolicited.xml");
    const ACS: &str = "https://gw.test.example/admin/v1/auth/saml/corp/acs";
    const SP: &str = "https://gw.test.example/admin/v1/auth/saml/corp/metadata";
    const IDP: &str = "https://idp.test.example/metadata";
    /// 2026-10-15T12:01:00Z.
    const NOW: i64 = 1_792_065_660;

    fn certs() -> Vec<Certificate> {
        let mut c = from_pem(include_str!("testdata/rsa-cert.pem")).unwrap();
        c.extend(from_pem(include_str!("testdata/ec-cert.pem")).unwrap());
        c
    }

    fn check(
        xml: &str,
        request_id: Option<&str>,
        now: i64,
        certificates: &[Certificate],
    ) -> Result<Assertion, SamlError> {
        validate(
            xml,
            &Expected {
                idp_entity_id: IDP,
                sp_entity_id: SP,
                acs_url: ACS,
                request_id,
                certificates,
                now,
                skew: 60,
            },
        )
    }

    fn ok(xml: &str) -> Result<Assertion, SamlError> {
        check(xml, Some("_req1"), NOW, &certs())
    }

    #[test]
    fn signed_assertions_from_another_implementation_verify() {
        let a = ok(RSA).unwrap();
        assert_eq!(a.id, "_a1");
        assert_eq!(a.name_id.as_deref(), Some("ada@corp.example"));
        assert_eq!(
            a.name_id_format.as_deref(),
            Some("urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress")
        );
        assert_eq!(a.values("groups"), ["admins", "eng & ops"]);
        assert_eq!(a.values("email"), ["ada@corp.example"]);
        assert_eq!(a.session_index.as_deref(), Some("_s1"));
        assert!(a.authn_context.unwrap().ends_with("PasswordProtectedTransport"));
        assert_eq!(a.valid_until, NOW + 4 * 60 + 60);
        for other in [EC, SHA512_COMMENTS, BOTH] {
            assert_eq!(ok(other).unwrap().id, "_a1");
        }
    }

    #[test]
    fn a_changed_or_unsigned_assertion_is_refused() {
        let changed = RSA.replacen(
            ">ada@corp.example</saml:NameID>",
            ">eve@corp.example</saml:NameID>",
            1,
        );
        assert_eq!(ok(&changed).unwrap_err(), SamlError::BadSignature);
        let start = RSA.find("<ds:Signature").unwrap();
        let end = RSA.find("</ds:Signature>").unwrap() + "</ds:Signature>".len();
        let unsigned = format!("{}{}", &RSA[..start], &RSA[end..]);
        assert_eq!(ok(&unsigned).unwrap_err(), SamlError::Unsigned);
        let other = from_pem(include_str!("testdata/other-cert.pem")).unwrap();
        assert_eq!(
            check(RSA, Some("_req1"), NOW, &other).unwrap_err(),
            SamlError::BadSignature
        );
        let sha1 = RSA.replace(
            "http://www.w3.org/2001/04/xmldsig-more#rsa-sha256",
            "http://www.w3.org/2000/09/xmldsig#rsa-sha1",
        );
        assert_eq!(ok(&sha1).unwrap_err(), SamlError::UnsupportedAlgorithm);
        let tampered_response = BOTH.replacen(
            r#"IssueInstant="2026-10-15T12:00:00Z""#,
            r#"IssueInstant="2026-10-15T12:00:01Z""#,
            1,
        );
        assert_eq!(ok(&tampered_response).unwrap_err(), SamlError::BadSignature);
    }

    #[test]
    fn wrapped_assertions_are_refused() {
        let start = RSA.find("<saml:Assertion").unwrap();
        let end = RSA.find("</saml:Assertion>").unwrap() + "</saml:Assertion>".len();
        let original = &RSA[start..end];
        let evil = original.replace(
            ">ada@corp.example</saml:NameID>",
            ">eve@corp.example</saml:NameID>",
        );
        let two = format!("{}{evil}{}", &RSA[..start], &RSA[start..]);
        assert_eq!(ok(&two).unwrap_err(), SamlError::Malformed);
        let hidden = format!(
            "{}<samlp:Extensions>{original}</samlp:Extensions>{evil}{}",
            &RSA[..start],
            &RSA[end..]
        );
        assert_eq!(ok(&hidden).unwrap_err(), SamlError::BadSignature);
        let renamed = format!(
            "{}{}{}",
            &RSA[..start],
            evil.replacen(r#"ID="_a1""#, r#"ID="_evil""#, 1),
            &RSA[end..]
        );
        assert_eq!(ok(&renamed).unwrap_err(), SamlError::BadSignature);
        let split = RSA.replacen(
            ">ada@corp.example</saml:NameID>",
            ">ada@corp<!---->.example</saml:NameID>",
            1,
        );
        assert_eq!(
            ok(&split).unwrap_err(),
            SamlError::Malformed,
            "the signature still verifies without comments, but a value split by one is refused"
        );
    }

    #[test]
    fn time_audience_recipient_and_request_are_checked() {
        assert_eq!(
            check(RSA, Some("_req1"), NOW + 600, &certs()).unwrap_err(),
            SamlError::Expired
        );
        assert_eq!(
            check(RSA, Some("_req1"), NOW - 600, &certs()).unwrap_err(),
            SamlError::NotYetValid
        );
        assert!(
            check(RSA, Some("_req1"), NOW + 4 * 60 + 30, &certs()).is_ok(),
            "within the skew"
        );
        assert_eq!(
            check(RSA, Some("_req2"), NOW, &certs()).unwrap_err(),
            SamlError::InResponseTo
        );
        assert_eq!(
            check(RSA, None, NOW, &certs()).unwrap_err(),
            SamlError::Unsolicited
        );
        assert_eq!(check(UNSOLICITED, None, NOW, &certs()).unwrap().id, "_a1");
        assert_eq!(
            check(UNSOLICITED, Some("_req1"), NOW, &certs()).unwrap_err(),
            SamlError::InResponseTo
        );
        let e = |sp: &'static str, acs: &'static str, idp: &'static str| {
            let c = certs();
            validate(
                RSA,
                &Expected {
                    idp_entity_id: idp,
                    sp_entity_id: sp,
                    acs_url: acs,
                    request_id: Some("_req1"),
                    certificates: &c,
                    now: NOW,
                    skew: 60,
                },
            )
            .unwrap_err()
        };
        assert_eq!(e("https://other.example/sp", ACS, IDP), SamlError::Audience);
        assert_eq!(e(SP, "https://other.example/acs", IDP), SamlError::Destination);
        assert_eq!(e(SP, ACS, "https://other-idp.example"), SamlError::Issuer);
    }

    #[test]
    fn refusals_and_odd_documents() {
        let refused = RSA.replace(
            "urn:oasis:names:tc:SAML:2.0:status:Success",
            "urn:oasis:names:tc:SAML:2.0:status:Requester",
        );
        assert_eq!(
            ok(&refused).unwrap_err(),
            SamlError::Status(Some("Requester".to_owned()))
        );
        assert_eq!(
            ok("<!DOCTYPE r [<!ENTITY a 'b'>]><r/>").unwrap_err(),
            SamlError::Malformed
        );
        assert_eq!(ok("<a/>").unwrap_err(), SamlError::Malformed);
        let encrypted = RSA.replacen(
            "<saml:Assertion ",
            r#"<saml:EncryptedAssertion/><saml:Assertion "#,
            1,
        );
        assert_eq!(ok(&encrypted).unwrap_err(), SamlError::Encrypted);
    }
}

//! `auth.admin.sso.saml`: the identity providers an operator configured, compiled for sign-ins,
//! and the rules the schema cannot state (one source of the identity provider; inline metadata and
//! certificates that can be read).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use antirion_config::scalar::parse_duration;
use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::Value;

use super::cert::from_pem;
use super::metadata::{IdpDescriptor, parse_idp_metadata};

/// The clock skew allowed when none is set, in seconds.
const DEFAULT_SKEW: i64 = 60;
/// The largest clock skew a provider may allow, in seconds.
const MAX_SKEW: i64 = 300;

/// Where the identity provider's endpoint and keys come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdpSource {
    /// Fetched from `url` and refreshed every `refresh`. `insecure` lets the fetch use plain HTTP or a
    /// TLS profile that does not verify the server, which the metadata's certificates otherwise need.
    MetadataUrl {
        url: String,
        refresh: Duration,
        insecure: bool,
    },
    /// Inline metadata, or the entity id, endpoint and certificates given one by one.
    Fixed(IdpDescriptor),
}

/// One configured SAML identity provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    pub display_name: Option<String>,
    pub idp: IdpSource,
    /// Configured; `None` derives it from the console's origin.
    pub sp_entity_id: Option<String>,
    /// The NameID format URN the requests ask for.
    pub name_id_format: &'static str,
    /// The attribute holding the email address; `None` reads the NameID.
    pub email_attribute: Option<String>,
    pub allowed_email_domains: Option<Vec<String>>,
    pub jit_provisioning: bool,
    /// `(attribute, value, role, scope)`; every match adds a binding.
    pub role_mappings: Vec<(String, String, String, String)>,
    /// `(role, scope)` for a provisioned user no mapping matched.
    pub default_role_bindings: Vec<(String, String)>,
    pub allow_idp_initiated: bool,
    /// Seconds.
    pub clock_skew: i64,
}

/// The NameID format URN of a `name_id_format` value.
#[must_use]
pub fn name_id_format(v: &str) -> &'static str {
    match v {
        "persistent" => "urn:oasis:names:tc:SAML:2.0:nameid-format:persistent",
        "transient" => "urn:oasis:names:tc:SAML:2.0:nameid-format:transient",
        "unspecified" => "urn:oasis:names:tc:SAML:1.1:nameid-format:unspecified",
        _ => "urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress",
    }
}

fn text(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn strings(v: Option<&Value>) -> impl Iterator<Item = &str> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

/// Why an `idp` block cannot be used: the field to blame (empty for the block), the code and why.
type Refusal = (&'static str, DetailCode, String);

/// The identity provider of an `idp` block, or why it cannot be used.
fn idp(v: &Value) -> Result<IdpSource, Refusal> {
    let sources = [
        v.get("metadata_url").is_some(),
        v.get("metadata").is_some(),
        ["entity_id", "sso_url", "certificates"]
            .iter()
            .any(|k| v.get(k).is_some()),
    ];
    if sources.iter().filter(|s| **s).count() != 1 {
        return Err((
            "",
            DetailCode::Conflict,
            "set exactly one of metadata_url, metadata, or entity_id with sso_url and certificates"
                .to_owned(),
        ));
    }
    if let Some(url) = text(v, "metadata_url") {
        let refresh = v
            .get("metadata_refresh")
            .and_then(Value::as_str)
            .and_then(parse_duration)
            .unwrap_or(Duration::from_secs(86_400));
        let insecure = v
            .get("allow_insecure_metadata")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return Ok(IdpSource::MetadataUrl {
            url,
            refresh,
            insecure,
        });
    }
    if let Some(xml) = text(v, "metadata") {
        return parse_idp_metadata(&xml)
            .map(IdpSource::Fixed)
            .map_err(|e| ("metadata", DetailCode::InvalidFormat, e.to_string()));
    }
    let mut certificates = Vec::new();
    for pem in strings(v.get("certificates")) {
        certificates
            .extend(from_pem(pem).map_err(|e| ("certificates", DetailCode::InvalidFormat, e.to_string()))?);
    }
    match (text(v, "entity_id"), text(v, "sso_url")) {
        (Some(entity_id), Some(sso_url)) if !certificates.is_empty() => Ok(IdpSource::Fixed(IdpDescriptor {
            entity_id,
            sso_url,
            certificates,
        })),
        (None, _) => Err((
            "entity_id",
            DetailCode::Required,
            "required with sso_url and certificates".to_owned(),
        )),
        (_, None) => Err((
            "sso_url",
            DetailCode::Required,
            "required with entity_id and certificates".to_owned(),
        )),
        _ => Err((
            "certificates",
            DetailCode::Required,
            "required with entity_id and sso_url".to_owned(),
        )),
    }
}

fn provider(p: &Value) -> Option<Provider> {
    let idp = idp(p.get("idp")?).ok()?;
    let role_mappings = p
        .get("role_mappings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            Some((
                text(m, "attribute")?,
                text(m, "value").unwrap_or_default(),
                text(m, "role")?,
                text(m, "scope")?,
            ))
        })
        .collect();
    let default_role_bindings = p
        .get("default_role_bindings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|b| Some((text(b, "role")?, text(b, "scope")?)))
        .collect();
    Some(Provider {
        display_name: text(p, "display_name"),
        idp,
        sp_entity_id: text(p, "sp_entity_id"),
        name_id_format: name_id_format(p.get("name_id_format").and_then(Value::as_str).unwrap_or("email")),
        email_attribute: text(p, "email_attribute"),
        allowed_email_domains: p
            .get("allowed_email_domains")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect()),
        jit_provisioning: p.get("jit_provisioning").and_then(Value::as_bool).unwrap_or(true),
        role_mappings,
        default_role_bindings,
        allow_idp_initiated: p
            .get("allow_idp_initiated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        clock_skew: p
            .get("clock_skew")
            .and_then(Value::as_str)
            .and_then(parse_duration)
            .map_or(DEFAULT_SKEW, |d| {
                i64::try_from(d.as_secs()).map_or(MAX_SKEW, |s| s.min(MAX_SKEW))
            }),
    })
}

/// The SAML providers of a validated, resolved document.
#[must_use]
pub fn compile(doc: &Value) -> BTreeMap<String, Arc<Provider>> {
    doc.pointer("/auth/admin/sso/saml")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, p)| Some((name.clone(), Arc::new(provider(p)?))))
        .collect()
}

/// The rules of `auth.admin.sso.saml` the schema cannot state.
#[must_use]
pub fn check_document(doc: &Value) -> Vec<ConfigError> {
    let mut errors = Vec::new();
    for (name, p) in doc
        .pointer("/auth/admin/sso/saml")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let at = DocPath::root()
            .key("auth")
            .key("admin")
            .key("sso")
            .key("saml")
            .key(name)
            .key("idp");
        let Some(block) = p.get("idp") else { continue };
        if let Err((field, code, why)) = idp(block) {
            let path = if field.is_empty() { at } else { at.key(field) };
            errors.push(ConfigError::new(&path, code, why));
        }
    }
    errors
}

/// Whether `url` is plain HTTP.
#[must_use]
pub fn is_plaintext(url: &str) -> bool {
    url.get(..7)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http://"))
}

/// Metadata URLs that would be fetched over plain HTTP: the metadata carries the certificates every
/// assertion is checked against, so outside test mode it comes over HTTPS unless the provider sets
/// `allow_insecure_metadata`.
#[must_use]
pub fn check_transport(doc: &Value, test_mode: bool) -> Vec<ConfigError> {
    if test_mode {
        return Vec::new();
    }
    doc.pointer("/auth/admin/sso/saml")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, p)| {
            let idp = p.get("idp")?;
            let url = idp.get("metadata_url").and_then(Value::as_str)?;
            let allowed = idp
                .get("allow_insecure_metadata")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            (is_plaintext(url) && !allowed).then(|| {
                let at = DocPath::root()
                    .key("auth")
                    .key("admin")
                    .key("sso")
                    .key("saml")
                    .key(name)
                    .key("idp")
                    .key("metadata_url");
                ConfigError::new(
                    &at,
                    DetailCode::InvalidFormat,
                    "must be an https URL: the metadata holds the identity provider's signing \
                     certificates (set allow_insecure_metadata to fetch it over http)",
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::cert::tests::PEM;
    use super::*;

    fn doc(idp: Value) -> Value {
        json!({"auth": {"admin": {"sso": {"saml": {"corp": {"idp": idp, "role_mappings": [
            {"attribute": "groups", "value": "admins", "role": "admin", "scope": "gateway"}]}}}}}})
    }

    fn paths(idp: Value) -> Vec<(String, DetailCode)> {
        check_document(&doc(idp))
            .into_iter()
            .map(|e| (e.path, e.code))
            .collect()
    }

    #[test]
    fn one_source_of_the_identity_provider() {
        let at = "auth.admin.sso.saml.corp.idp";
        assert_eq!(
            paths(json!({"metadata_url": "https://idp/m", "entity_id": "e"})),
            [(at.to_owned(), DetailCode::Conflict)]
        );
        assert_eq!(paths(json!({})), [(at.to_owned(), DetailCode::Conflict)]);
        assert_eq!(
            paths(json!({"entity_id": "e", "certificates": [PEM]})),
            [(format!("{at}.sso_url"), DetailCode::Required)]
        );
        assert_eq!(
            paths(json!({"entity_id": "e", "sso_url": "https://idp/sso", "certificates": ["junk"]})),
            [(format!("{at}.certificates"), DetailCode::InvalidFormat)]
        );
        assert_eq!(
            paths(json!({"metadata": "<a/>"})),
            [(format!("{at}.metadata"), DetailCode::InvalidFormat)]
        );
        assert!(
            paths(json!({"entity_id": "e", "sso_url": "https://idp/sso", "certificates": [PEM]})).is_empty()
        );
    }

    #[test]
    fn providers_compile() {
        let d = doc(json!({"entity_id": "https://idp", "sso_url": "https://idp/sso", "certificates": [PEM]}));
        let p = &compile(&d)["corp"];
        assert_eq!(
            p.name_id_format,
            "urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress"
        );
        assert_eq!(p.clock_skew, 60);
        assert!(p.jit_provisioning && !p.allow_idp_initiated);
        assert_eq!(p.role_mappings[0].0, "groups");
        assert!(matches!(&p.idp, IdpSource::Fixed(d) if d.certificates.len() == 1));
        let url = compile(&doc(
            json!({"metadata_url": "https://idp/m", "metadata_refresh": "1h"}),
        ));
        assert_eq!(
            url["corp"].idp,
            IdpSource::MetadataUrl {
                url: "https://idp/m".to_owned(),
                refresh: Duration::from_secs(3600),
                insecure: false,
            }
        );
    }

    #[test]
    fn clock_skew_stays_within_five_minutes() {
        let skew = |v: &str| {
            let mut d = doc(json!({"metadata_url": "https://idp/m"}));
            d["auth"]["admin"]["sso"]["saml"]["corp"]["clock_skew"] = json!(v);
            compile(&d)["corp"].clock_skew
        };
        assert_eq!(skew("2m"), 120);
        assert_eq!(skew("1h"), 300);
        assert_eq!(skew("not a duration"), 60);
    }

    #[test]
    fn plaintext_metadata_needs_test_mode_or_the_opt_in() {
        let plain = doc(json!({"metadata_url": "HTTP://idp/m"}));
        let errors = check_transport(&plain, false);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path, "auth.admin.sso.saml.corp.idp.metadata_url");
        assert!(check_transport(&plain, true).is_empty());
        assert!(check_transport(&doc(json!({"metadata_url": "https://idp/m"})), false).is_empty());
        let opted = doc(json!({"metadata_url": "http://idp/m", "allow_insecure_metadata": true}));
        assert!(check_transport(&opted, false).is_empty());
        assert!(matches!(
            &compile(&opted)["corp"].idp,
            IdpSource::MetadataUrl { insecure: true, .. }
        ));
    }
}

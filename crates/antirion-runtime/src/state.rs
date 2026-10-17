//! The stored dynamic configuration: the document and the metadata of every resource.

use std::collections::BTreeMap;

use antirion_config::model::{self, COLLECTIONS, CollectionDef, SINGLETONS};
use antirion_core::ResourceUid;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// The verifier of a key created through the Admin API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyCredential {
    pub kid: String,
    pub verifier: String,
    pub hint: String,
    /// The secret a rotation replaced, while its grace period lasts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<PreviousCredential>,
}

/// The verifier prefix of an imported legacy key: the SHA-256 of the legacy secret, in hex.
pub const IMPORTED_PREFIX: &str = "sha256:";

impl KeyCredential {
    /// The SHA-256 digest an imported key authenticates by; `None` for a generated secret.
    #[must_use]
    pub fn imported_digest(&self) -> Option<&str> {
        self.verifier.strip_prefix(IMPORTED_PREFIX)
    }
}

impl PreviousCredential {
    /// The SHA-256 digest of an imported secret a rotation replaced.
    #[must_use]
    pub fn imported_digest(&self) -> Option<&str> {
        self.verifier.strip_prefix(IMPORTED_PREFIX)
    }
}

/// A rotated-out secret that stays valid until `valid_until` (RFC 3339).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousCredential {
    pub kid: String,
    pub verifier: String,
    pub valid_until: String,
}

/// Read-only metadata of a resource or a singleton.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceMeta {
    pub etag: String,
    pub generation: u64,
    pub created_at: String,
    pub updated_at: String,
    pub managed_by: String,
    #[serde(with = "uid_hex")]
    pub uid: ResourceUid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<KeyCredential>,
    /// The principal that created the resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// Why the gateway disabled the resource itself (a key whose owner was deleted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    /// When a Model's lifecycle state last changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_at: Option<String>,
    /// When each provider credential was last set: `credential`, or a key's name.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub credentials_rotated_at: std::collections::BTreeMap<String, String>,
    /// When a key's secret was last rotated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotated_at: Option<String>,
}

mod uid_hex {
    use antirion_core::ResourceUid;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(uid: &ResourceUid, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&uid.to_hex())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<ResourceUid, D::Error> {
        let s = String::deserialize(d)?;
        ResourceUid::from_hex(&s).ok_or_else(|| serde::de::Error::custom("invalid uid"))
    }
}

/// Metadata of every resource (collection name → resource name) and every singleton.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub resources: BTreeMap<String, BTreeMap<String, ResourceMeta>>,
    pub settings: BTreeMap<String, ResourceMeta>,
}

/// The dynamic configuration of one generation.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigState {
    pub generation: u64,
    /// The document as stored: reference text and literal secrets as written.
    pub doc: Value,
    pub meta: Metadata,
}

impl ConfigState {
    /// The configuration of a gateway that has never been configured.
    #[must_use]
    pub fn empty() -> ConfigState {
        let mut doc = Map::new();
        doc.insert(
            "version".into(),
            Value::from(antirion_config::schema::CURRENT_VERSION),
        );
        ConfigState {
            generation: 0,
            doc: Value::Object(doc),
            meta: Metadata::default(),
        }
    }

    #[must_use]
    pub fn resource(&self, c: &CollectionDef, name: &str) -> Option<(&Value, Option<&ResourceMeta>)> {
        let spec = model::collection_map(&self.doc, c)?.get(name)?;
        Some((spec, self.meta.resources.get(c.name).and_then(|m| m.get(name))))
    }

    /// `sha256:<hex>` of the canonical document.
    #[must_use]
    pub fn hash(&self) -> String {
        format!(
            "sha256:{}",
            hex(&Sha256::digest(serde_json::to_vec(&self.doc).unwrap_or_default()))
        )
    }

    /// Every resource uid, for sweeping runtime state of deleted resources.
    pub fn live_uids(&self) -> impl Iterator<Item = ResourceUid> + '_ {
        self.meta
            .resources
            .values()
            .flat_map(|m| m.values().map(|r| r.uid))
    }

    /// The incarnation of a resource, or a fixed value when it has none recorded.
    #[must_use]
    pub fn uid_of(&self, collection: &str, name: &str) -> ResourceUid {
        self.meta
            .resources
            .get(collection)
            .and_then(|m| m.get(name))
            .map_or(ResourceUid::GATEWAY, |r| r.uid)
    }
}

/// The strong ETag of a spec: a quoted digest of its canonical JSON.
#[must_use]
pub fn etag_of(spec: &Value) -> String {
    let mut sorted = spec.clone();
    sorted.sort_all_objects();
    let digest = Sha256::digest(serde_json::to_vec(&sorted).unwrap_or_default());
    format!("\"{}\"", hex(digest.get(..10).unwrap_or_default()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Every resource in `doc` as (collection, name, spec).
pub fn resources(doc: &Value) -> Vec<(&'static CollectionDef, String, Value)> {
    let mut out = Vec::new();
    for c in COLLECTIONS {
        for (name, spec) in model::collection_map(doc, c).into_iter().flatten() {
            out.push((c, name.clone(), spec.clone()));
        }
    }
    out
}

/// Every present singleton in `doc` as (name, settings).
pub fn singletons(doc: &Value) -> Vec<(&'static str, Value)> {
    SINGLETONS
        .iter()
        .filter_map(|s| model::singleton_value(doc, s).map(|v| (s.name, v)))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn etags_follow_the_spec_only() {
        let a = etag_of(&json!({"target": "d", "description": "x"}));
        assert_eq!(a, etag_of(&json!({"description": "x", "target": "d"})));
        assert_ne!(a, etag_of(&json!({"target": "d"})));
        assert!(a.starts_with('"') && a.ends_with('"') && !a.starts_with("W/"));
    }

    #[test]
    fn metadata_round_trips_through_json() {
        let mut meta = Metadata::default();
        meta.resources.entry("keys".into()).or_default().insert(
            "k".into(),
            ResourceMeta {
                etag: "\"x\"".into(),
                generation: 3,
                created_at: "t".into(),
                updated_at: "t".into(),
                managed_by: "api".into(),
                uid: ResourceUid::random(),
                credential: Some(KeyCredential {
                    kid: "abc".into(),
                    verifier: "v".into(),
                    hint: "h".into(),
                    previous: None,
                }),
                created_by: Some("bootstrap".into()),
                disabled_reason: None,
                lifecycle_at: None,
                credentials_rotated_at: std::collections::BTreeMap::new(),
                rotated_at: None,
            },
        );
        let text = serde_json::to_string(&meta).unwrap();
        assert_eq!(serde_json::from_str::<Metadata>(&text).unwrap(), meta);
    }

    #[test]
    fn hash_has_the_published_shape() {
        let h = ConfigState::empty().hash();
        assert!(h.starts_with("sha256:") && h.len() == 7 + 64);
    }
}

//! Modules kept in registries: `oci://<registry>/<repository>[:<tag>|@<digest>]` names the
//! repository, and the module is its blob whose digest is the configured `sha256`, so the tag is
//! never resolved and the bytes cannot change behind the configuration. The runtime fetches the blob
//! (through its egress client) before the configuration naming it is compiled and hands the bytes
//! here; compiling reads them back by digest.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// The registry and repository of an `oci://` module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OciRef {
    /// Host and optional port.
    pub registry: String,
    pub repository: String,
}

impl OciRef {
    /// Parses an `oci://` module reference; `None` for anything else.
    #[must_use]
    pub fn parse(module: &str) -> Option<OciRef> {
        let rest = module.strip_prefix("oci://")?;
        let (registry, path) = rest.split_once('/')?;
        let repository = match path.split_once('@') {
            Some((repo, _)) => repo,
            None => match path.rsplit_once(':') {
                Some((repo, tag)) if !tag.contains('/') => repo,
                _ => path,
            },
        };
        let valid = !registry.is_empty()
            && !repository.is_empty()
            && repository.split('/').all(|c| {
                !c.is_empty()
                    && c.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
            });
        valid.then(|| OciRef {
            registry: registry.to_owned(),
            repository: repository.to_owned(),
        })
    }

    /// The URL of the blob with this digest (registry API v2).
    #[must_use]
    pub fn blob_url(&self, sha256: &str) -> String {
        format!(
            "https://{}/v2/{}/blobs/sha256:{sha256}",
            self.registry, self.repository
        )
    }
}

fn store() -> &'static Mutex<HashMap<String, Arc<[u8]>>> {
    static FETCHED: OnceLock<Mutex<HashMap<String, Arc<[u8]>>>> = OnceLock::new();
    FETCHED.get_or_init(Mutex::default)
}

/// Keeps fetched module bytes under their digest; false (nothing kept) when they do not have it.
pub fn provide(sha256: &str, bytes: Vec<u8>) -> bool {
    if super::digest(&bytes) != sha256 {
        return false;
    }
    if let Ok(mut m) = store().lock() {
        m.insert(sha256.to_owned(), Arc::from(bytes));
    }
    true
}

/// Whether the module with this digest was fetched.
#[must_use]
pub fn fetched(sha256: &str) -> bool {
    store().lock().is_ok_and(|m| m.contains_key(sha256))
}

pub(super) fn bytes(sha256: &str) -> Option<Arc<[u8]>> {
    store().lock().ok()?.get(sha256).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_name_the_repository() {
        let r = OciRef::parse("oci://registry.example:5000/antirion/house-style:1.2.0").unwrap();
        assert_eq!(r.registry, "registry.example:5000");
        assert_eq!(r.repository, "antirion/house-style");
        assert_eq!(
            r.blob_url("ab"),
            "https://registry.example:5000/v2/antirion/house-style/blobs/sha256:ab"
        );
        let d = OciRef::parse("oci://r.example/x@sha256:00").unwrap();
        assert_eq!(d.repository, "x");
        assert_eq!(OciRef::parse("oci://r.example/x").unwrap().repository, "x");
        assert!(OciRef::parse("file:///x.wasm").is_none());
        assert!(OciRef::parse("oci://r.example").is_none());
        assert!(OciRef::parse("oci://r.example/Upper").is_none());
    }

    #[test]
    fn only_bytes_with_the_digest_are_kept() {
        let bytes = b"\0asm-not-really".to_vec();
        let d = super::super::digest(&bytes);
        assert!(!provide(&"0".repeat(64), bytes.clone()));
        assert!(provide(&d, bytes));
        assert!(fetched(&d));
    }
}

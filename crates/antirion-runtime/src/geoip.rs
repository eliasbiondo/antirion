//! Local GeoIP lookups from a MaxMind-format database (`observability.geoip.database`): a record's
//! `client_geo` is `{country, region, city}` of its client IP. The database is read when a snapshot
//! is built (on the builder's blocking thread) and kept while the file is unchanged; the gateway
//! never asks a remote service.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use maxminddb::{PathElement, Reader};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// An opened database.
pub struct GeoDb {
    reader: Reader<Vec<u8>>,
}

impl std::fmt::Debug for GeoDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeoDb").finish_non_exhaustive()
    }
}

type Cached = (PathBuf, Option<SystemTime>, Arc<GeoDb>);

/// The database last opened, reused while its path and modification time stay the same.
static LAST: Mutex<Option<Cached>> = Mutex::new(None);

impl GeoDb {
    /// Opens `path`, or returns the copy already open when the file did not change. Blocking.
    pub fn load(path: &str) -> Option<Arc<GeoDb>> {
        let path = Path::new(path);
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if let Some((p, m, db)) = LAST.lock().as_ref()
            && p == path
            && *m == modified
        {
            return Some(Arc::clone(db));
        }
        let reader = match Reader::open_readfile(path) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "the GeoIP database cannot be read");
                return None;
            }
        };
        let db = Arc::new(GeoDb { reader });
        *LAST.lock() = Some((path.to_owned(), modified, Arc::clone(&db)));
        Some(db)
    }

    /// `{country, region, city}` of `ip`; `None` when the database knows nothing of it.
    #[must_use]
    pub fn locate(&self, ip: IpAddr) -> Option<Value> {
        let found = self.reader.lookup(ip).ok()?;
        if !found.has_data() {
            return None;
        }
        let text = |path: &[PathElement<'_>]| -> Value {
            found
                .decode_path::<String>(path)
                .ok()
                .flatten()
                .map_or(Value::Null, Value::String)
        };
        let country = text(&[PathElement::Key("country"), PathElement::Key("iso_code")]);
        let region = text(&[
            PathElement::Key("subdivisions"),
            PathElement::Index(0),
            PathElement::Key("iso_code"),
        ]);
        let city = text(&[
            PathElement::Key("city"),
            PathElement::Key("names"),
            PathElement::Key("en"),
        ]);
        Some(json!({"country": country, "region": region, "city": city}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_database_is_no_lookup() {
        assert!(GeoDb::load("/nonexistent/antirion-geo.mmdb").is_none());
    }
}

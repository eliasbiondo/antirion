//! Invalidation finer than the epochs in the key: by Model, route, deployment, key, end user, tag
//! or metadata value, and by age. Each is a cutoff time kept beside the epochs (and shared through
//! the store like them): an entry written at or before the cutoff of a dimension value it carries
//! is dead from that moment on every node, whatever the backend still holds. Nodes then remove the
//! dead entries from their backend and index off the request path.

use crate::entry::Entry;
use crate::meta::dim;

const PREFIX: &str = "cut:";
const OLDER_THAN: &str = "older_than";

/// What an invalidation removes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Every entry.
    All,
    /// The entries carrying a dimension value (`model/m`), only those of one scope digest when
    /// given.
    Dim { dim: String, digest: Option<String> },
    /// Every entry written at or before the cutoff.
    OlderThan,
}

impl Target {
    /// The entries of a dimension value.
    #[must_use]
    pub fn dim(dimension: &str, value: &str) -> Target {
        Target::Dim {
            dim: dim(dimension, value),
            digest: None,
        }
    }

    /// The name of its cutoff among the epochs; `None` for [`Target::All`], which the gateway
    /// epoch covers.
    #[must_use]
    pub fn cutoff_key(&self) -> Option<String> {
        match self {
            Target::All => None,
            Target::Dim { dim, digest: None } => Some(format!("{PREFIX}{dim}")),
            Target::Dim { dim, digest: Some(d) } => Some(format!("{PREFIX}{d}@{dim}")),
            Target::OlderThan => Some(format!("{PREFIX}{OLDER_THAN}")),
        }
    }

    /// The target a stored cutoff name stands for.
    #[must_use]
    pub fn of_cutoff_key(key: &str) -> Option<Target> {
        let rest = key.strip_prefix(PREFIX)?;
        if rest == OLDER_THAN {
            return Some(Target::OlderThan);
        }
        match rest.split_once('@') {
            Some((digest, d)) if !digest.contains('/') => Some(Target::Dim {
                dim: d.to_owned(),
                digest: Some(digest.to_owned()),
            }),
            _ => Some(Target::Dim {
                dim: rest.to_owned(),
                digest: None,
            }),
        }
    }
}

/// Whether `entry` was written at or before a cutoff that applies to it; `cutoff` reads one by
/// name.
pub fn dead(entry: &Entry, cutoff: impl Fn(&str) -> Option<i64>) -> bool {
    let reached = |key: &str| cutoff(key).is_some_and(|c| entry.written_at_ms <= c);
    if reached(&format!("{PREFIX}{OLDER_THAN}")) {
        return true;
    }
    let Some(meta) = &entry.meta else {
        return false;
    };
    meta.dims(&entry.deployment)
        .iter()
        .any(|d| reached(&format!("{PREFIX}{d}")) || reached(&format!("{PREFIX}{}@{d}", meta.digest)))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::meta::EntryMeta;

    fn entry(at: i64) -> Entry {
        Entry {
            written_at_ms: at,
            ttl_ms: 1_000,
            content_type: String::new(),
            body: None,
            events: None,
            provider: String::new(),
            deployment: "d".into(),
            model: String::new(),
            upstream_status: 200,
            upstream_dialect: None,
            usage: None,
            meta: Some(EntryMeta {
                digest: "abc".into(),
                model: "m".into(),
                metadata: [("v".to_owned(), "1".to_owned())].into(),
                ..EntryMeta::default()
            }),
        }
    }

    #[test]
    fn cutoffs_round_trip_and_apply() {
        for t in [
            Target::dim("model", "m"),
            Target::OlderThan,
            Target::Dim {
                dim: "metadata/v=1".into(),
                digest: Some("abc".into()),
            },
        ] {
            assert_eq!(Target::of_cutoff_key(&t.cutoff_key().unwrap()), Some(t));
        }
        let mut cuts: HashMap<String, i64> = HashMap::new();
        let get = |c: &HashMap<String, i64>| {
            let c = c.clone();
            move |k: &str| c.get(k).copied()
        };
        assert!(!dead(&entry(10), get(&cuts)));
        cuts.insert(Target::dim("model", "m").cutoff_key().unwrap(), 10);
        assert!(dead(&entry(10), get(&cuts)));
        assert!(!dead(&entry(11), get(&cuts)));
        cuts.clear();
        let scoped = Target::Dim {
            dim: "metadata/v=1".into(),
            digest: Some("abc".into()),
        };
        cuts.insert(scoped.cutoff_key().unwrap(), 20);
        assert!(dead(&entry(15), get(&cuts)));
        cuts.clear();
        cuts.insert(Target::OlderThan.cutoff_key().unwrap(), 5);
        assert!(dead(&entry(5), get(&cuts)) && !dead(&entry(6), get(&cuts)));
    }
}

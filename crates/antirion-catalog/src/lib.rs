//! The model catalog: capability values, time-to-first-token hints and prices per model pattern,
//! from the bundled files and the document's `catalog` block, which extends or replaces them.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use serde_json::{Map, Value};

/// The bundled prices, `{version, models: {pattern: price}}`.
pub const BUNDLED_PRICES: &str = include_str!("../catalog/prices.json");

/// The bundled capability values and hints, `{version, models: {pattern: entry}}`.
const BUNDLED_MODELS: &str = include_str!("../catalog/models.json");

/// One pattern's data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    /// Capability flags the entry sets.
    pub capabilities: Map<String, Value>,
    /// The time-to-first-token hint.
    pub ttft: Option<Duration>,
    /// The price, as written (`Price` of the limits engine reads it).
    pub price: Option<Value>,
}

impl Entry {
    fn from_value(v: &Value) -> Entry {
        Entry {
            capabilities: v
                .get("capabilities")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            ttft: v
                .get("ttft")
                .and_then(Value::as_str)
                .and_then(antirion_config::scalar::parse_duration),
            price: v.get("price").filter(|p| p.is_object()).cloned(),
        }
    }

    /// The entry as the catalog read shows it.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("capabilities".into(), Value::Object(self.capabilities.clone()));
        m.insert(
            "ttft".into(),
            self.ttft.map_or(Value::Null, |d| {
                Value::from(antirion_config::scalar::format_duration(d))
            }),
        );
        m.insert("price".into(), self.price.clone().unwrap_or(Value::Null));
        Value::Object(m)
    }
}

/// Where an entry of the effective catalog comes from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Bundled,
    Runtime,
}

impl Source {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Bundled => "bundled",
            Source::Runtime => "runtime",
        }
    }
}

/// How the document's catalog combines with the bundled one.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Extend,
    Replace,
}

impl Mode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Extend => "extend",
            Mode::Replace => "replace",
        }
    }
}

/// What the document's `catalog` block says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Runtime {
    pub mode: Mode,
    pub version: Option<String>,
    pub entries: usize,
}

/// What the bundled catalog says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundled {
    /// The version of the capability file.
    pub version: String,
    /// The version of the bundled prices.
    pub price_version: String,
    pub entries: usize,
}

/// The effective catalog of one configuration generation.
#[derive(Clone, Debug)]
pub struct Catalog {
    entries: BTreeMap<String, (Arc<Entry>, Source)>,
    /// Patterns ending in `*`, without it, longest first.
    prefixes: Vec<String>,
    pub bundled: Bundled,
    pub runtime: Option<Runtime>,
}

fn version_of(v: &Value) -> String {
    v.get("version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The bundled entries: the capability file's, with the bundled prices merged in by pattern.
fn bundled_entries() -> (BTreeMap<String, Entry>, Bundled) {
    let models: Value = serde_json::from_str(BUNDLED_MODELS).unwrap_or(Value::Null);
    let prices: Value = serde_json::from_str(BUNDLED_PRICES).unwrap_or(Value::Null);
    let mut out: BTreeMap<String, Entry> = models
        .get("models")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), Entry::from_value(v)))
        .collect();
    for (k, p) in prices
        .get("models")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        out.entry(k.clone()).or_default().price = Some(p.clone());
    }
    let info = Bundled {
        version: version_of(&models),
        price_version: version_of(&prices),
        entries: out.len(),
    };
    (out, info)
}

static BUNDLED: LazyLock<(BTreeMap<String, Entry>, Bundled)> = LazyLock::new(bundled_entries);

impl Catalog {
    /// The bundled catalog alone.
    #[must_use]
    pub fn bundled() -> Catalog {
        Catalog::from_block(None)
    }

    /// The effective catalog of a resolved document.
    #[must_use]
    pub fn from_doc(doc: &Value) -> Catalog {
        Catalog::from_block(doc.get("catalog").filter(|c| c.is_object()))
    }

    fn from_block(block: Option<&Value>) -> Catalog {
        let (bundled, info) = &*BUNDLED;
        let mode = match block.and_then(|b| b.get("mode")).and_then(Value::as_str) {
            Some("replace") => Mode::Replace,
            _ => Mode::Extend,
        };
        let mut entries: BTreeMap<String, (Arc<Entry>, Source)> = if mode == Mode::Replace {
            BTreeMap::new()
        } else {
            bundled
                .iter()
                .map(|(k, e)| (k.clone(), (Arc::new(e.clone()), Source::Bundled)))
                .collect()
        };
        let own = block.and_then(|b| b.get("models")).and_then(Value::as_object);
        for (k, v) in own.into_iter().flatten() {
            entries.insert(k.clone(), (Arc::new(Entry::from_value(v)), Source::Runtime));
        }
        let mut prefixes: Vec<String> = entries
            .keys()
            .filter_map(|k| k.strip_suffix('*').map(str::to_owned))
            .collect();
        prefixes.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        Catalog {
            entries,
            prefixes,
            bundled: info.clone(),
            runtime: block.map(|b| Runtime {
                mode,
                version: b.get("version").and_then(Value::as_str).map(str::to_owned),
                entries: own.map_or(0, Map::len),
            }),
        }
    }

    /// The entry of a deployment of a `provider_type` provider serving `base_model`: the exact
    /// `<type>/<model>` pattern, then the exact `<model>`, then the longest `*` prefix (a
    /// type-qualified prefix before a bare one of the same length).
    #[must_use]
    pub fn lookup(&self, provider_type: &str, base_model: &str) -> Option<(&Entry, Source)> {
        self.find(provider_type, base_model, false)
    }

    /// [`Catalog::lookup`] for a provider of a generic type, which serves other vendors' models
    /// under their own ids: when its own type has no entry, an entry another type qualifies for
    /// the same model id counts as a bare one, after the bare ones (the first by pattern when
    /// several types have one).
    #[must_use]
    pub fn lookup_any_type(&self, provider_type: &str, base_model: &str) -> Option<(&Entry, Source)> {
        self.find(provider_type, base_model, true)
    }

    fn find(&self, provider_type: &str, base_model: &str, any_type: bool) -> Option<(&Entry, Source)> {
        let qualified = format!("{provider_type}/{base_model}");
        let other_type = || {
            any_type
                .then(|| {
                    self.entries
                        .iter()
                        .find(|(k, _)| k.split_once('/').is_some_and(|(_, m)| m == base_model))
                        .map(|(_, v)| v)
                })
                .flatten()
        };
        if let Some((e, s)) = self
            .entries
            .get(&qualified)
            .or_else(|| self.entries.get(base_model))
            .or_else(other_type)
        {
            return Some((e.as_ref(), *s));
        }
        let mut best: Option<(usize, u8, &str)> = None;
        for p in &self.prefixes {
            let split = p.split_once('/');
            let rank = match split {
                Some((t, m)) if t == provider_type && base_model.starts_with(m) => Some((m.len(), 2)),
                _ if base_model.starts_with(p.as_str()) => Some((p.len(), 1)),
                Some((_, m)) if any_type && base_model.starts_with(m) => Some((m.len(), 0)),
                _ => None,
            };
            let Some((matched, rank)) = rank else {
                continue;
            };
            if best.is_none_or(|(len, was, _)| matched > len || (matched == len && rank > was)) {
                best = Some((matched, rank, p.as_str()));
            }
        }
        let (_, _, p) = best?;
        self.entries.get(&format!("{p}*")).map(|(e, s)| (e.as_ref(), *s))
    }

    /// Every entry, by pattern.
    pub fn items(&self) -> impl Iterator<Item = (&str, &Entry, Source)> {
        self.entries
            .iter()
            .map(|(k, (e, s))| (k.as_str(), e.as_ref(), *s))
    }

    /// The prices of the effective catalog, by pattern.
    pub fn prices(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.entries
            .iter()
            .filter_map(|(k, (e, _))| e.price.as_ref().map(|p| (k.as_str(), p)))
    }
}

impl Default for Catalog {
    fn default() -> Catalog {
        Catalog::bundled()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_bundled_catalog_carries_prices() {
        let c = Catalog::bundled();
        assert!(!c.bundled.price_version.is_empty());
        let (e, s) = c.lookup("openai", "gpt-4o").unwrap();
        assert_eq!(s, Source::Bundled);
        assert!(e.price.is_some());
        assert!(c.runtime.is_none());
    }

    #[test]
    fn runtime_entries_extend_or_replace() {
        let doc = json!({"catalog": {"version": "v2", "models": {
            "openai/gpt-4o": {"capabilities": {"max_output_tokens": 100}, "ttft": "90s"},
            "acme-*": {"capabilities": {"tools": false}},
            "acme-large-*": {"capabilities": {"tools": true}},
            "openai/acme-*": {"capabilities": {"vision": false}},
        }}});
        let c = Catalog::from_doc(&doc);
        let (e, s) = c.lookup("openai", "gpt-4o").unwrap();
        assert_eq!(s, Source::Runtime);
        assert_eq!(e.ttft, Some(Duration::from_secs(90)));
        assert!(
            e.price.is_none(),
            "a runtime entry replaces the bundled one whole"
        );
        assert!(
            c.lookup("openai", "gpt-4o-mini")
                .is_some_and(|(_, s)| s == Source::Bundled)
        );
        let tools = |t: &str, m: &str| c.lookup(t, m).map(|(e, _)| e.capabilities.clone());
        assert_eq!(tools("anthropic", "acme-large-1").unwrap()["tools"], json!(true));
        assert_eq!(tools("anthropic", "acme-small").unwrap()["tools"], json!(false));
        assert_eq!(tools("openai", "acme-small").unwrap()["vision"], json!(false));
        assert!(c.lookup("anthropic", "other").is_none());
        let rt = c.runtime.unwrap();
        assert_eq!(
            (rt.mode, rt.version.as_deref(), rt.entries),
            (Mode::Extend, Some("v2"), 4)
        );

        let replaced = Catalog::from_doc(&json!({"catalog": {"mode": "replace", "models": {"m": {}}}}));
        assert!(replaced.lookup("openai", "gpt-4o-mini").is_none());
        assert!(replaced.lookup("openai", "m").is_some());
        assert_eq!(replaced.prices().count(), 0);
    }

    #[test]
    fn a_generic_type_reads_the_entry_of_its_model_id_under_any_type() {
        let doc = json!({"catalog": {"models": {
            "openai/gpt-4o": {"capabilities": {"context_window": 128_000}},
            "anthropic/claude-x*": {"capabilities": {"context_window": 200_000}},
            "llama-3": {"capabilities": {"context_window": 8_192}},
            "openai/llama-3": {"capabilities": {"context_window": 1}},
            "openai_compatible/own": {"capabilities": {"context_window": 2}},
            "openai/own": {"capabilities": {"context_window": 3}},
        }}});
        let c = Catalog::from_doc(&doc);
        let window = |any: bool, m: &str| {
            let found = if any {
                c.lookup_any_type("openai_compatible", m)
            } else {
                c.lookup("openai_compatible", m)
            };
            found.map(|(e, _)| e.capabilities["context_window"].clone())
        };
        assert_eq!(
            window(false, "gpt-4o"),
            None,
            "a typed entry of another type is not the type's own"
        );
        assert_eq!(window(true, "gpt-4o"), Some(json!(128_000)));
        assert_eq!(window(true, "claude-x-1"), Some(json!(200_000)));
        assert_eq!(
            window(true, "llama-3"),
            Some(json!(8_192)),
            "a bare entry before another type's"
        );
        assert_eq!(window(true, "own"), Some(json!(2)), "the type's own entry first");
        assert_eq!(window(true, "unknown"), None);
    }
}

//! The metrics registry: the catalogue's families, with label values truncated and the series of each
//! family capped (extra samples go to an `_overflow` series), configurable histogram buckets, and the
//! Prometheus and OpenMetrics text expositions.

pub mod catalog;
mod expo;
pub mod labels;
pub mod otlp;
mod process;

use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, BuildHasherDefault, Hasher, RandomState};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::{Mutex, RwLock};

pub use self::catalog::{Fam, Kind};
use self::catalog::{FamilyDef, KEPT_ON_OVERFLOW};
pub use self::expo::Format;
use self::labels::Extra;
pub use self::labels::{CustomLabel, LabelSource};

/// Facts about the running build, exported as `antirion_build_info`.
#[derive(Clone, Debug)]
pub struct BuildInfo {
    pub version: String,
    pub commit: String,
    pub rustc: String,
    pub semconv: String,
    pub test_mode: bool,
}

/// Limits and overrides from `observability.metrics`.
#[derive(Clone, Debug, PartialEq)]
pub struct MetricsConfig {
    pub max_label_value_bytes: usize,
    pub max_series_per_family: usize,
    /// Bucket overrides by family name.
    pub buckets: BTreeMap<String, Vec<f64>>,
    /// Families left out of the exposition.
    pub exclude: Vec<String>,
    /// `tenant_labels`: a subset of organization, team and project.
    pub tenant_labels: Vec<String>,
    pub custom_labels: Vec<Arc<CustomLabel>>,
}

impl Default for MetricsConfig {
    fn default() -> MetricsConfig {
        MetricsConfig {
            max_label_value_bytes: 128,
            max_series_per_family: 10_000,
            buckets: BTreeMap::new(),
            exclude: Vec::new(),
            tenant_labels: Vec::new(),
            custom_labels: Vec::new(),
        }
    }
}

pub(crate) const OVERFLOW: &str = "_overflow";

/// The most labels a family has, configured labels included.
const MAX_LABELS: usize = 32;

#[derive(Clone, Debug)]
enum Series {
    Value(f64),
    Histogram { counts: Vec<u64>, sum: f64, count: u64 },
}

/// A family's series, found by a hash of their label values so that a sample for an existing
/// series allocates nothing.
#[derive(Debug, Default)]
struct SeriesMap {
    by_hash: HashMap<u64, Bucket, BuildHasherDefault<Prehashed>>,
    len: usize,
}

/// The series whose label values share one hash.
type Bucket = Vec<(Vec<String>, Series)>;

/// The map's keys are already seeded hashes of the label values: they are used as they are.
#[derive(Default)]
struct Prehashed(u64);

impl Hasher for Prehashed {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(*b);
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

impl SeriesMap {
    fn get_mut(&mut self, hash: u64, labels: &[&str]) -> Option<&mut Series> {
        self.by_hash
            .get_mut(&hash)?
            .iter_mut()
            .find(|(k, _)| k.iter().map(String::as_str).eq(labels.iter().copied()))
            .map(|(_, s)| s)
    }

    fn insert(&mut self, hash: u64, key: Vec<String>, series: Series) {
        self.len += 1;
        self.by_hash.entry(hash).or_default().push((key, series));
    }

    fn iter(&self) -> impl Iterator<Item = (&Vec<String>, &Series)> {
        self.by_hash.values().flatten().map(|(k, s)| (k, s))
    }

    fn clear(&mut self) {
        self.by_hash.clear();
        self.len = 0;
    }
}

#[derive(Debug)]
struct FamilyState {
    buckets: Vec<f64>,
    series: SeriesMap,
    /// Configured labels after the catalogue's.
    extras: Vec<Extra>,
}

impl FamilyState {
    fn names(&self, def: &FamilyDef) -> Vec<String> {
        def.labels
            .iter()
            .map(|l| (*l).to_owned())
            .chain(self.extras.iter().map(|e| e.name().to_owned()))
            .collect()
    }
}

#[derive(Debug)]
struct Inner {
    families: Vec<Mutex<FamilyState>>,
    config: RwLock<MetricsConfig>,
    /// `max_label_value_bytes` and `max_series_per_family` of `config`, read on every sample.
    max_label_bytes: AtomicUsize,
    max_series: AtomicUsize,
    /// A random seed for the hash of label values.
    seed: u64,
}

/// The process's metrics. There is no global registry: the runtime owns this one, and clones share it.
#[derive(Clone, Debug)]
pub struct Metrics {
    inner: Arc<Inner>,
}

/// `s` cut to at most `max` bytes at a UTF-8 boundary.
#[must_use]
pub fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or_default()
}

fn unix_now() -> f64 {
    use antirion_core::Clock;
    antirion_core::SystemClock.now().0 as f64 / 1e6
}

impl Metrics {
    #[must_use]
    pub fn new(build: &BuildInfo) -> Metrics {
        let families = Fam::ALL
            .iter()
            .map(|f| {
                Mutex::new(FamilyState {
                    buckets: f.def().buckets.to_vec(),
                    series: SeriesMap::default(),
                    extras: Vec::new(),
                })
            })
            .collect();
        let config = MetricsConfig::default();
        let m = Metrics {
            inner: Arc::new(Inner {
                families,
                max_label_bytes: AtomicUsize::new(config.max_label_value_bytes),
                max_series: AtomicUsize::new(config.max_series_per_family),
                config: RwLock::new(config),
                seed: RandomState::new().hash_one(0u64),
            }),
        };
        let test_mode = build.test_mode.to_string();
        m.set(
            Fam::BuildInfo,
            &[
                &build.version,
                &build.commit,
                &build.rustc,
                &build.semconv,
                &test_mode,
            ],
            1.0,
        );
        m.set(Fam::StartTime, &[], unix_now().floor());
        m
    }

    /// Applies new limits and bucket overrides. A histogram whose buckets change starts afresh.
    pub fn configure(&self, config: &MetricsConfig) {
        let mut current = self.inner.config.write();
        if *current == *config {
            return;
        }
        for (fam, state) in Fam::ALL.iter().zip(&self.inner.families) {
            let def = fam.def();
            let extras = labels::extras(*fam, &config.tenant_labels, &config.custom_labels);
            {
                let mut st = state.lock();
                if st.extras != extras {
                    st.extras = extras;
                    st.series.clear();
                }
            }
            if def.kind != Kind::Histogram {
                continue;
            }
            let wanted = config
                .buckets
                .get(def.name)
                .cloned()
                .unwrap_or_else(|| def.buckets.to_vec());
            let mut st = state.lock();
            if st.buckets != wanted {
                st.buckets = wanted;
                st.series.clear();
            }
        }
        self.inner
            .max_label_bytes
            .store(config.max_label_value_bytes, Ordering::Relaxed);
        self.inner
            .max_series
            .store(config.max_series_per_family, Ordering::Relaxed);
        *current = config.clone();
    }

    /// Adds `v` to a counter or gauge series.
    pub fn add(&self, fam: Fam, labels: &[&str], v: f64) {
        self.add_for(fam, labels, None, v);
    }

    /// Adds `v` to a counter series of a family that may carry configured labels, read from `src`.
    pub fn add_for(&self, fam: Fam, labels: &[&str], src: Option<&LabelSource<'_>>, v: f64) {
        self.update(fam, labels, src, |s, _| {
            if let Series::Value(x) = s {
                *x += v;
            }
        });
    }

    /// Sets a gauge series.
    pub fn set(&self, fam: Fam, labels: &[&str], v: f64) {
        self.update(fam, labels, None, |s, _| {
            if let Series::Value(x) = s {
                *x = v;
            }
        });
    }

    /// Records one histogram observation.
    pub fn observe(&self, fam: Fam, labels: &[&str], v: f64) {
        self.observe_for(fam, labels, None, v);
    }

    /// Records one observation in a family that may carry configured labels, read from `src`.
    pub fn observe_for(&self, fam: Fam, labels: &[&str], src: Option<&LabelSource<'_>>, v: f64) {
        self.update(fam, labels, src, |s, buckets| {
            if let Series::Histogram { counts, sum, count } = s {
                let i = buckets.iter().position(|b| v <= *b).unwrap_or(buckets.len());
                if let Some(c) = counts.get_mut(i) {
                    *c += 1;
                }
                *sum += v;
                *count += 1;
            }
        });
    }

    fn hash(&self, labels: &[&str]) -> u64 {
        labels.iter().fold(self.inner.seed, |h, l| {
            xxhash_rust::xxh3::xxh3_64_with_seed(l.as_bytes(), h)
        })
    }

    fn update(
        &self,
        fam: Fam,
        labels: &[&str],
        src: Option<&LabelSource<'_>>,
        f: impl FnOnce(&mut Series, &[f64]),
    ) {
        let def = fam.def();
        if labels.len() != def.labels.len() || labels.len() > MAX_LABELS {
            debug_assert!(false, "{}: wrong label count", def.name);
            return;
        }
        let max_bytes = self.inner.max_label_bytes.load(Ordering::Relaxed);
        let cap = self.inner.max_series.load(Ordering::Relaxed);
        let Some(state) = self.inner.families.get(fam.index()) else {
            return;
        };
        let mut overflowed = false;
        {
            let mut st = state.lock();
            let FamilyState {
                buckets,
                series,
                extras,
            } = &mut *st;
            let mut cut = [""; MAX_LABELS];
            let all = labels.iter().copied().chain(extras.iter().map(|e| e.value(src)));
            let mut n = 0;
            for (c, l) in cut.iter_mut().zip(all) {
                *c = truncate_utf8(l, max_bytes);
                n += 1;
            }
            let cut = cut.get(..n).unwrap_or_default();
            let hash = self.hash(cut);
            if let Some(s) = series.get_mut(hash, cut) {
                f(s, buckets);
            } else {
                let key: Vec<String> = cut.iter().map(|v| (*v).to_owned()).collect();
                let (hash, key) = if fam != Fam::SeriesOverflow && series.len >= cap {
                    overflowed = true;
                    let key = overflow_key(def, key);
                    let refs: Vec<&str> = key.iter().map(String::as_str).collect();
                    (self.hash(&refs), key)
                } else {
                    (hash, key)
                };
                let refs: Vec<&str> = key.iter().map(String::as_str).collect();
                if let Some(s) = series.get_mut(hash, &refs) {
                    f(s, buckets);
                } else {
                    let mut fresh = match def.kind {
                        Kind::Histogram => Series::Histogram {
                            counts: vec![0; buckets.len() + 1],
                            sum: 0.0,
                            count: 0,
                        },
                        _ => Series::Value(0.0),
                    };
                    f(&mut fresh, buckets);
                    series.insert(hash, key, fresh);
                }
            }
        }
        if overflowed {
            self.add(Fam::SeriesOverflow, &[def.name], 1.0);
        }
    }

    /// The text exposition in `format`.
    #[must_use]
    pub fn render(&self, format: Format) -> String {
        let exclude = self.inner.config.read().exclude.clone();
        let mut out = String::new();
        for (fam, state) in Fam::ALL.iter().zip(&self.inner.families) {
            let def = fam.def();
            if exclude.iter().any(|e| e == def.name) {
                continue;
            }
            let st = state.lock();
            let mut series: Vec<(&Vec<String>, &Series)> = st.series.iter().collect();
            series.sort_by(|a, b| a.0.cmp(b.0));
            let names = st.names(def);
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            expo::family(&mut out, format, def, &names, &st.buckets, &series);
        }
        process::render(&mut out, format);
        if format == Format::OpenMetrics {
            out.push_str("# EOF\n");
        }
        out
    }

    /// Every exposed family's series, for OTLP export.
    #[must_use]
    pub fn families(&self) -> Vec<otlp::FamilyData> {
        let exclude = self.inner.config.read().exclude.clone();
        let mut out = Vec::new();
        for (fam, state) in Fam::ALL.iter().zip(&self.inner.families) {
            let def = fam.def();
            if exclude.iter().any(|e| e == def.name) {
                continue;
            }
            let st = state.lock();
            let mut series: Vec<(Vec<String>, otlp::Point)> = st
                .series
                .iter()
                .map(|(k, s)| {
                    let p = match s {
                        Series::Value(v) => otlp::Point::Value(*v),
                        Series::Histogram { counts, sum, count } => otlp::Point::Histogram {
                            counts: counts.clone(),
                            sum: *sum,
                            count: *count,
                        },
                    };
                    (k.clone(), p)
                })
                .collect();
            series.sort_by(|a, b| a.0.cmp(&b.0));
            out.push(otlp::FamilyData {
                name: def.name,
                help: def.help,
                kind: def.kind,
                labels: st.names(def),
                buckets: st.buckets.clone(),
                series,
            });
        }
        out
    }

    /// The sum of a family's series whose labels include `labels`, for tests and status reads.
    #[must_use]
    pub fn sum(&self, fam: Fam, labels: &[(&str, &str)]) -> f64 {
        let def = fam.def();
        let Some(state) = self.inner.families.get(fam.index()) else {
            return 0.0;
        };
        let st = state.lock();
        st.series
            .iter()
            .filter(|(k, _)| {
                labels.iter().all(|(name, value)| {
                    def.labels
                        .iter()
                        .position(|l| l == name)
                        .and_then(|i| k.get(i))
                        .is_some_and(|v| v == value)
                })
            })
            .map(|(_, s)| match s {
                Series::Value(v) => *v,
                Series::Histogram { count, .. } => *count as f64,
            })
            .sum()
    }
}

fn overflow_key(def: &FamilyDef, key: Vec<String>) -> Vec<String> {
    key.into_iter()
        .enumerate()
        .map(|(i, value)| match def.labels.get(i) {
            Some(name) if KEPT_ON_OVERFLOW.contains(name) => value,
            _ => OVERFLOW.to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics::new(&BuildInfo {
            version: "0.1.0".into(),
            commit: "abc".into(),
            rustc: "1.98.0".into(),
            semconv: "core-1.41.1".into(),
            test_mode: true,
        })
    }

    fn lines<'a>(text: &'a str, prefix: &str) -> Vec<&'a str> {
        text.lines().filter(|l| l.starts_with(prefix)).collect()
    }

    #[test]
    fn build_info_is_one_series() {
        let text = metrics().render(Format::Prometheus);
        let series = lines(&text, "antirion_build_info{");
        assert_eq!(series.len(), 1, "{text}");
        assert!(series[0].contains("test_mode=\"true\"") && series[0].ends_with(" 1"));
        assert!(text.contains("# TYPE antirion_build_info gauge"));
        assert!(text.contains("# TYPE antirion_requests_total counter"));
    }

    #[test]
    fn openmetrics_names_counters_without_total_and_ends_with_eof() {
        let m = metrics();
        m.add(Fam::Retries, &["p", "d", "server_error"], 1.0);
        let text = m.render(Format::OpenMetrics);
        assert!(text.contains("# TYPE antirion_retries counter"));
        assert!(text.contains(
            "antirion_retries_total{provider=\"p\",deployment=\"d\",failure_class=\"server_error\"} 1"
        ));
        assert!(text.ends_with("# EOF\n"));
    }

    #[test]
    fn histograms_are_cumulative_with_inf() {
        let m = metrics();
        m.observe(Fam::Overhead, &["chat_completions", "false"], 0.0003);
        m.observe(Fam::Overhead, &["chat_completions", "false"], 7.0);
        let text = m.render(Format::Prometheus);
        assert!(text.contains("antirion_overhead_seconds_bucket{operation=\"chat_completions\",stream=\"false\",le=\"0.0005\"} 1"));
        assert!(text.contains(
            "antirion_overhead_seconds_bucket{operation=\"chat_completions\",stream=\"false\",le=\"+Inf\"} 2"
        ));
        assert!(
            text.contains(
                "antirion_overhead_seconds_count{operation=\"chat_completions\",stream=\"false\"} 2"
            )
        );
    }

    #[test]
    fn configured_buckets_replace_the_defaults() {
        let m = metrics();
        m.observe(Fam::Overhead, &["x", "false"], 0.002);
        let mut c = MetricsConfig::default();
        c.buckets
            .insert("antirion_overhead_seconds".into(), vec![0.001, 0.01]);
        m.configure(&c);
        m.observe(Fam::Overhead, &["x", "false"], 0.002);
        let text = m.render(Format::Prometheus);
        let buckets = lines(&text, "antirion_overhead_seconds_bucket");
        assert_eq!(buckets.len(), 3, "{text}");
        assert!(buckets[1].contains("le=\"0.01\"") && buckets[1].ends_with(" 1"));
    }

    #[test]
    fn long_values_are_truncated_and_the_cap_overflows() {
        let m = metrics();
        m.configure(&MetricsConfig {
            max_series_per_family: 2,
            ..MetricsConfig::default()
        });
        let long = "é".repeat(100);
        m.add(Fam::UpstreamAttempts, &["p", "d", &long, "success", "200"], 1.0);
        m.add(Fam::UpstreamAttempts, &["p", "d", "b", "success", "200"], 1.0);
        m.add(Fam::UpstreamAttempts, &["p", "d", "c", "success", "200"], 1.0);
        m.add(
            Fam::UpstreamAttempts,
            &["p", "d", "e", "upstream_5xx", "500"],
            1.0,
        );
        let text = m.render(Format::Prometheus);
        assert!(text.contains(&format!("upstream_model=\"{}\"", "é".repeat(64))));
        assert!(text.contains(
            "antirion_upstream_attempts_total{provider=\"_overflow\",deployment=\"_overflow\",upstream_model=\"_overflow\",outcome=\"success\",status_code=\"_overflow\"} 1"
        ));
        assert_eq!(
            m.sum(
                Fam::SeriesOverflow,
                &[("family", "antirion_upstream_attempts_total")]
            ),
            2.0
        );
    }

    #[test]
    fn excluded_families_are_not_exposed() {
        let m = metrics();
        m.configure(&MetricsConfig {
            exclude: vec!["antirion_build_info".into()],
            ..MetricsConfig::default()
        });
        assert!(!m.render(Format::Prometheus).contains("antirion_build_info"));
    }

    #[test]
    fn every_family_fits_the_label_bound() {
        assert!(Fam::ALL.iter().all(|f| f.def().labels.len() <= MAX_LABELS));
    }

    #[test]
    fn samples_of_one_series_accumulate() {
        let m = metrics();
        for _ in 0..3 {
            m.add(Fam::Retries, &["p", "d", "server_error"], 1.0);
        }
        m.add(Fam::Retries, &["p", "d2", "server_error"], 1.0);
        assert_eq!(m.sum(Fam::Retries, &[("deployment", "d")]), 3.0);
        assert_eq!(m.sum(Fam::Retries, &[]), 4.0);
    }

    #[test]
    fn truncation_keeps_utf8() {
        assert_eq!(truncate_utf8("aé", 2), "a");
        assert_eq!(truncate_utf8("abc", 5), "abc");
    }
}

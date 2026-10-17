//! Per-repetition metrics from the raw files, and the cell summary over repetitions (median, min, max).

use std::collections::BTreeMap;
use std::path::Path;

use hdrhistogram::Histogram;
use serde_json::{Map, Value, json};

use crate::model::{CellDef, CountsFile, CpuFile, Validity, read_json};
use crate::{ReportError, hdr};

/// Flat metrics of one repetition, keyed by dotted path (`overhead_us.p99`).
pub type Metrics = BTreeMap<String, f64>;

pub const PERCENTILES: [(&str, f64); 4] = [("p50", 0.5), ("p90", 0.9), ("p99", 0.99), ("p99_9", 0.999)];

#[allow(clippy::cast_precision_loss)]
fn stats(h: &Histogram<u64>) -> Vec<(&'static str, f64)> {
    let mut out: Vec<(&str, f64)> = PERCENTILES
        .iter()
        .map(|(name, q)| (*name, h.value_at_quantile(*q) as f64))
        .collect();
    out.push(("max", h.max() as f64));
    out.push(("mean", h.mean()));
    out
}

fn put_stats(m: &mut Metrics, prefix: &str, h: &Histogram<u64>) {
    if h.is_empty() {
        return;
    }
    for (name, v) in stats(h) {
        m.insert(format!("{prefix}.{name}"), v);
    }
}

fn put_diff(m: &mut Metrics, prefix: &str, gw: &Histogram<u64>, direct: &Histogram<u64>) {
    if gw.is_empty() || direct.is_empty() {
        return;
    }
    for ((name, a), (_, b)) in stats(gw).into_iter().zip(stats(direct)) {
        m.insert(format!("{prefix}.{name}"), a - b);
    }
}

fn opt_hdr(dir: &Path, name: &str) -> Result<Option<Histogram<u64>>, ReportError> {
    let path = dir.join(name);
    if path.exists() {
        hdr::read(&path).map(Some)
    } else {
        Ok(None)
    }
}

/// RSS samples of `rss.csv`: steady (mean) and peak.
fn rss(dir: &Path, m: &mut Metrics) -> Result<(), ReportError> {
    let path = dir.join("rss.csv");
    if !path.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&path).map_err(|source| ReportError::Read { path, source })?;
    let samples: Vec<f64> = text
        .lines()
        .skip(1)
        .filter_map(|l| l.split(',').nth(1)?.trim().parse().ok())
        .collect();
    if samples.is_empty() {
        return Ok(());
    }
    #[allow(clippy::cast_precision_loss)]
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    m.insert("rss_bytes.steady".to_owned(), mean);
    m.insert(
        "rss_bytes.peak".to_owned(),
        samples.iter().copied().fold(0.0, f64::max),
    );
    Ok(())
}

#[allow(clippy::cast_precision_loss)]
fn counts(m: &mut Metrics, prefix: &str, c: &CountsFile) {
    m.insert(format!("{prefix}achieved_rps"), c.achieved_rps);
    m.insert(
        format!("{prefix}errors.non_2xx"),
        c.non_2xx.values().sum::<u64>() as f64,
    );
    m.insert(format!("{prefix}errors.connect_errors"), c.connect_errors as f64);
    m.insert(format!("{prefix}errors.timeouts"), c.timeouts as f64);
    m.insert(format!("{prefix}errors.total"), c.errors() as f64);
    if c.offered > 0 {
        m.insert(
            format!("{prefix}error_rate"),
            c.errors() as f64 / c.offered as f64,
        );
        m.insert(format!("{prefix}late_fraction"), c.late as f64 / c.offered as f64);
        m.insert(
            format!("{prefix}success_fraction"),
            c.ok as f64 / c.offered as f64,
        );
    }
}

/// The metrics of one repetition directory.
pub fn repetition(dir: &Path) -> Result<(Metrics, Validity), ReportError> {
    let mut m = Metrics::new();
    let gw = opt_hdr(dir, "gw.hdr")?;
    let direct = opt_hdr(dir, "direct.hdr")?;
    if let Some(g) = &gw {
        put_stats(&mut m, "latency_us", g);
    }
    if let Some(d) = &direct {
        put_stats(&mut m, "direct_latency_us", d);
    }
    if let (Some(g), Some(d)) = (&gw, &direct) {
        put_diff(&mut m, "overhead_us", g, d);
    }
    let (tg, td) = (opt_hdr(dir, "ttft-gw.hdr")?, opt_hdr(dir, "ttft-direct.hdr")?);
    if let Some(t) = &tg {
        put_stats(&mut m, "ttft_us", t);
    }
    if let (Some(g), Some(d)) = (&tg, &td) {
        put_diff(&mut m, "ttft_overhead_us", g, d);
    }
    if let (Some(g), Some(d)) = (opt_hdr(dir, "gap-gw.hdr")?, opt_hdr(dir, "gap-direct.hdr")?) {
        put_diff(&mut m, "inter_event_overhead_us", &g, &d);
    }
    if let Some(h) = opt_hdr(dir, "lag-gw.hdr")? {
        put_stats(&mut m, "lag_us", &h);
    }
    if let Some(h) = opt_hdr(dir, "overhead-header.hdr")? {
        put_stats(&mut m, "overhead_header_us", &h);
    }
    rss(dir, &mut m)?;
    let path = dir.join("counts.json");
    let mut completed = None;
    if path.exists() {
        let c: CountsFile = read_json(&path)?;
        completed = Some(c.ok);
        counts(&mut m, "", &c);
    }
    let path = dir.join("counts-direct.json");
    if path.exists() {
        counts(&mut m, "direct.", &read_json(&path)?);
    }
    let path = dir.join("cpu.json");
    if path.exists() {
        let cpu: CpuFile = read_json(&path)?;
        m.insert("generator_cpu_pct".to_owned(), cpu.generator_cpu_pct);
        if let Some(p) = cpu.mock_cpu_pct {
            m.insert("mock_cpu_pct".to_owned(), p);
        }
        let done = completed.unwrap_or(cpu.completed);
        if let Some(s) = cpu.gateway_cpu_s
            && done > 0
        {
            #[allow(clippy::cast_precision_loss)]
            m.insert("cpu_us_per_request".to_owned(), s * 1e6 / done as f64);
        }
    }
    let path = dir.join("extra.json");
    if path.exists() {
        let extra: BTreeMap<String, f64> = read_json(&path)?;
        m.extend(extra);
    }
    let path = dir.join("validity.json");
    let validity = if path.exists() {
        read_json(&path)?
    } else {
        Validity {
            valid: false,
            reasons: vec!["validity.json is missing".to_owned()],
        }
    };
    Ok((m, validity))
}

/// Median, min and max of each metric over the repetitions.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub stats: BTreeMap<String, (f64, f64, f64)>,
    pub valid: bool,
    pub reasons: Vec<String>,
    pub repetitions: usize,
    /// Why the cell was not run here, when it was not.
    pub skipped: Option<String>,
}

impl Summary {
    #[must_use]
    pub fn median(&self, metric: &str) -> Option<f64> {
        self.stats.get(metric).map(|s| s.0)
    }

    /// The spread (max − min) of the direct run's latency at a percentile, the noise floor of
    /// the overhead at that percentile.
    #[must_use]
    pub fn direct_spread(&self, pct: &str) -> Option<f64> {
        self.stats
            .get(&format!("direct_latency_us.{pct}"))
            .map(|(_, lo, hi)| hi - lo)
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        v.get(n / 2).copied().unwrap_or(f64::NAN)
    } else {
        (v.get(n / 2 - 1).copied().unwrap_or(f64::NAN) + v.get(n / 2).copied().unwrap_or(f64::NAN)) / 2.0
    }
}

/// Summarises the repetition directories of a cell (`rep-1`, `rep-2`, …).
pub fn cell(dir: &Path) -> Result<Summary, ReportError> {
    let skipped = dir.join("skipped.json");
    if skipped.exists() {
        let v: Value = read_json(&skipped)?;
        let reason = v
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("not run")
            .to_owned();
        return Ok(Summary {
            reasons: vec![reason.clone()],
            skipped: Some(reason),
            ..Summary::default()
        });
    }
    let mut reps: Vec<(Metrics, Validity)> = Vec::new();
    let mut n = 1;
    while dir.join(format!("rep-{n}")).is_dir() {
        reps.push(repetition(&dir.join(format!("rep-{n}")))?);
        n += 1;
    }
    let mut values: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for (m, _) in &reps {
        for (k, v) in m {
            values.entry(k.clone()).or_default().push(*v);
        }
    }
    let stats = values
        .into_iter()
        .map(|(k, v)| {
            let lo = v.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            (k, (median(v), lo, hi))
        })
        .collect();
    let mut reasons: Vec<String> = Vec::new();
    for (i, (_, v)) in reps.iter().enumerate() {
        for r in &v.reasons {
            reasons.push(format!("rep-{}: {r}", i + 1));
        }
    }
    Ok(Summary {
        stats,
        valid: !reps.is_empty() && reps.iter().all(|(_, v)| v.valid),
        reasons: if reps.is_empty() {
            vec!["no repetition was recorded".to_owned()]
        } else {
            reasons
        },
        repetitions: reps.len(),
        skipped: None,
    })
}

fn insert_path(root: &mut Map<String, Value>, path: &str, value: Value) {
    let mut parts = path.split('.').peekable();
    let mut cur = root;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            cur.insert(part.to_owned(), value);
            return;
        }
        let next = cur
            .entry(part.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if !next.is_object() {
            *next = Value::Object(Map::new());
        }
        let Value::Object(map) = next else { return };
        cur = map;
    }
}

fn number(v: f64) -> Value {
    if v.is_finite() {
        json!((v * 1000.0).round() / 1000.0)
    } else {
        Value::Null
    }
}

/// `summary.json` of a cell.
#[must_use]
pub fn to_json(def: &CellDef, s: &Summary) -> Value {
    let mut root = Map::new();
    root.insert("cell".to_owned(), json!(def.cell));
    root.insert("valid".to_owned(), json!(s.valid));
    root.insert("reasons".to_owned(), json!(s.reasons));
    root.insert("repetitions".to_owned(), json!(s.repetitions));
    root.insert("reference".to_owned(), json!(def.reference));
    if let Some(why) = &s.skipped {
        root.insert("skipped".to_owned(), json!(why));
    }
    for key in [
        "overhead_us",
        "ttft_overhead_us",
        "inter_event_overhead_us",
        "saturation_rps",
    ] {
        root.insert(key.to_owned(), Value::Null);
    }
    for (k, (med, lo, hi)) in &s.stats {
        insert_path(
            &mut root,
            k,
            json!({"median": number(*med), "min": number(*lo), "max": number(*hi)}),
        );
    }
    let spread: Map<String, Value> = PERCENTILES
        .iter()
        .map(|(p, _)| *p)
        .chain(["max", "mean"])
        .filter_map(|p| Some((p.to_owned(), number(s.direct_spread(p)?))))
        .collect();
    if !spread.is_empty() {
        root.insert("direct_spread_us".to_owned(), Value::Object(spread));
    }
    Value::Object(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medians() {
        assert!((median(vec![3.0, 1.0, 2.0]) - 2.0).abs() < 1e-9);
        assert!((median(vec![4.0, 1.0, 2.0, 3.0]) - 2.5).abs() < 1e-9);
        assert!(median(vec![]).is_nan());
    }

    #[test]
    fn nested_paths() {
        let mut m = Map::new();
        insert_path(&mut m, "a.b.c", json!(1));
        insert_path(&mut m, "a.d", json!(2));
        assert_eq!(Value::Object(m), json!({"a": {"b": {"c": 1}, "d": 2}}));
    }
}

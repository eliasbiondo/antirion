//! The text expositions: Prometheus 0.0.4 and OpenMetrics 1.0.

use std::fmt::Write;

use super::Series;
use super::catalog::{FamilyDef, Kind};

/// The exposition format a scrape asked for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Format {
    /// `text/plain; version=0.0.4`.
    Prometheus,
    /// `application/openmetrics-text; version=1.0.0`.
    OpenMetrics,
}

impl Format {
    /// The format an `Accept` header asks for.
    #[must_use]
    pub fn negotiate(accept: Option<&str>) -> Format {
        if accept.is_some_and(|a| a.to_ascii_lowercase().contains("application/openmetrics-text")) {
            Format::OpenMetrics
        } else {
            Format::Prometheus
        }
    }

    #[must_use]
    pub fn content_type(self) -> &'static str {
        match self {
            Format::Prometheus => "text/plain; version=0.0.4; charset=utf-8",
            Format::OpenMetrics => "application/openmetrics-text; version=1.0.0; charset=utf-8",
        }
    }
}

pub(super) fn number(v: f64) -> String {
    if v.is_infinite() {
        return if v > 0.0 { "+Inf".into() } else { "-Inf".into() };
    }
    format!("{v}")
}

fn escape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

fn labels(names: &[&str], values: &[String], extra: Option<(&str, &str)>) -> String {
    let mut parts: Vec<String> = names
        .iter()
        .zip(values)
        .map(|(n, v)| format!("{n}=\"{}\"", escape(v)))
        .collect();
    if let Some((n, v)) = extra {
        parts.push(format!("{n}=\"{v}\""));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{{{}}}", parts.join(","))
    }
}

pub(super) fn header(out: &mut String, format: Format, name: &str, help: &str, kind: &str) {
    let family = match (format, kind) {
        (Format::OpenMetrics, "counter") => name.strip_suffix("_total").unwrap_or(name),
        _ => name,
    };
    let _ = writeln!(out, "# HELP {family} {help}");
    let _ = writeln!(out, "# TYPE {family} {kind}");
}

pub(super) fn family(
    out: &mut String,
    format: Format,
    def: &FamilyDef,
    names: &[&str],
    buckets: &[f64],
    series: &[(&Vec<String>, &Series)],
) {
    let kind = match def.kind {
        Kind::Counter => "counter",
        Kind::Gauge => "gauge",
        Kind::Histogram => "histogram",
    };
    header(out, format, def.name, def.help, kind);
    for (values, s) in series {
        match s {
            Series::Value(v) => {
                let _ = writeln!(out, "{}{} {}", def.name, labels(names, values, None), number(*v));
            }
            Series::Histogram { counts, sum, count } => {
                let mut cumulative = 0u64;
                for (bound, n) in buckets.iter().copied().chain([f64::INFINITY]).zip(counts) {
                    cumulative += n;
                    let le = number(bound);
                    let _ = writeln!(
                        out,
                        "{}_bucket{} {cumulative}",
                        def.name,
                        labels(names, values, Some(("le", &le)))
                    );
                }
                let l = labels(names, values, None);
                let _ = writeln!(out, "{}_sum{l} {}", def.name, number(*sum));
                let _ = writeln!(out, "{}_count{l} {count}", def.name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_escapes() {
        assert_eq!(number(1.0), "1");
        assert_eq!(number(0.00005), "0.00005");
        assert_eq!(number(f64::INFINITY), "+Inf");
        assert_eq!(escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
        assert_eq!(
            Format::negotiate(Some("application/openmetrics-text; version=1.0.0")),
            Format::OpenMetrics
        );
        assert_eq!(Format::negotiate(Some("*/*")), Format::Prometheus);
    }
}

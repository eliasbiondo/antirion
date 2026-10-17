//! The files of a result directory, as the runner writes them and the report reads them.

use std::collections::BTreeMap;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::ReportError;

/// How a cell drives the gateway.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Constant arrival rate, gateway and direct runs alternating.
    Open,
    /// Fixed concurrency, gateway and direct runs alternating.
    Closed,
    /// The open-loop step ladder up to the saturation throughput.
    Saturation,
    /// Streams opened at a rate and held open for the window (idle streams).
    Hold,
    /// No load: resident memory of a started gateway.
    Idle,
    /// Process start to a ready probe.
    Startup,
    /// A one-field configuration apply and the time until every node serves it.
    Apply,
    /// A criterion micro-benchmark of the gateway's crates.
    Micro,
}

/// The step ladder of a saturation cell.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ladder {
    pub start: f64,
    pub step: f64,
    pub fine_step: f64,
    /// The largest rate tried.
    pub max: f64,
}

impl Default for Ladder {
    fn default() -> Self {
        Self {
            start: 1000.0,
            step: 1000.0,
            fine_step: 250.0,
            max: 200_000.0,
        }
    }
}

/// The load of a cell.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Level {
    /// Requests (or new streams) per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rps: Option<f64>,
    /// Streams open at once; the rate is this divided by the scripted stream length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrent_streams: Option<u64>,
    /// Closed-loop connections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<usize>,
    /// A run of this many requests instead of the standard window (soak).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_requests: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ladder: Option<Ladder>,
}

/// A threshold: a number, or a factor of another cell's metric.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Threshold {
    Value(f64),
    Relative { times: f64, of: String },
}

/// One pass condition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GateDef {
    /// A metric of the summary (`overhead_us.p99`), optionally of another cell (`C12/saturation_rps`).
    pub metric: String,
    /// `<=`, `<`, `>=`, `>` or `==`.
    pub op: String,
    pub threshold: Threshold,
    /// Subtracted from `metric` before the comparison (a difference between two cells).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minus: Option<String>,
    /// Compare the absolute value.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub abs: bool,
}

/// A cell definition (`cell.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellDef {
    pub cell: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default)]
    pub level: Level,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mock_profile: Option<String>,
    /// `h1` (default) or `h2c`.
    #[serde(default = "h1")]
    pub protocol: String,
    /// Gateway nodes behind client-side round robin.
    #[serde(default = "one")]
    pub nodes: usize,
    /// The topology the cell needs (`ref-16`, `t3-published`); absent: any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_topology: Option<String>,
    /// Share of streams the generator leaves after their first content event.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub disconnect_fraction: f64,
    /// Seconds of idleness after the load before the final samples (soak).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub idle_after_s: f64,
    /// Every exporter of the gateway points at a listener that never answers.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hanging_exporters: bool,
    /// The gateway machine of the topology this cell runs on, when it has several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// A cell whose repetitions alternate with this one's (A B A B …), each on a fresh gateway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternate_with: Option<String>,
    /// Overrides the standard warm-up and measurement windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measure_s: Option<f64>,
    /// Suites that run the cell; default: `release`, plus `compare` for cells marked `compare`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suites: Vec<String>,
    /// The micro-benchmark (`<package>/<bench>`), for `micro` cells.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bench: Option<String>,
    #[serde(default)]
    pub gates: Vec<GateDef>,
    /// Reported only; no pass condition.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub informational: bool,
    /// Also run for the comparison targets.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compare: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default)]
    pub topology: String,
    /// Whether the topology is a reference one; results elsewhere never claim a gate.
    #[serde(default)]
    pub reference: bool,
    #[serde(default)]
    pub repetitions: u32,
}

fn h1() -> String {
    "h1".to_owned()
}

fn one() -> usize {
    1
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

impl CellDef {
    /// The cell id of the matrix this cell (or variant `C19.1MiB`) belongs to.
    #[must_use]
    pub fn base_id(&self) -> &str {
        self.cell.split('.').next().unwrap_or(&self.cell)
    }
}

/// `counts.json` (and `counts-direct.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CountsFile {
    pub offered: u64,
    pub achieved_rps: f64,
    pub ok: u64,
    pub non_2xx: BTreeMap<String, u64>,
    pub connect_errors: u64,
    pub timeouts: u64,
    #[serde(default)]
    pub body_errors: u64,
    #[serde(default)]
    pub disconnects: u64,
    #[serde(default)]
    pub held: u64,
    /// Requests sent more than 1 ms after their intended time.
    #[serde(default)]
    pub late: u64,
    #[serde(default)]
    pub connections: u64,
}

impl CountsFile {
    #[must_use]
    pub fn errors(&self) -> u64 {
        self.non_2xx.values().sum::<u64>() + self.connect_errors + self.timeouts + self.body_errors
    }
}

/// `cpu.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuFile {
    /// Gateway CPU time over the window (every node), in seconds; null when it could not be read.
    pub gateway_cpu_s: Option<f64>,
    /// Percent of the machine's CPU capacity.
    pub generator_cpu_pct: f64,
    pub mock_cpu_pct: Option<f64>,
    pub completed: u64,
    /// `cgroup`, `proc` or `ps`.
    #[serde(default)]
    pub gateway_cpu_source: String,
}

/// `validity.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Validity {
    pub valid: bool,
    pub reasons: Vec<String>,
}

/// `run.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunManifest {
    pub schema_version: u32,
    pub suite: String,
    pub topology: String,
    pub target: String,
    pub target_version: String,
    pub target_commit: String,
    pub harness_commit: String,
    pub corpus_seed: u64,
    pub started_at: String,
    pub ended_at: String,
    pub command: Vec<String>,
    /// False on development topologies: no result of this run claims a gate.
    #[serde(default)]
    pub reference: bool,
    /// Why the run cannot be published, if it cannot (a debug build, shortened windows).
    #[serde(default)]
    pub not_publishable: Vec<String>,
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, ReportError> {
    let text = std::fs::read_to_string(path).map_err(|source| ReportError::Read {
        path: path.to_owned(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| ReportError::Json {
        path: path.to_owned(),
        source,
    })
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), ReportError> {
    let mut text = serde_json::to_string_pretty(value).map_err(|source| ReportError::Json {
        path: path.to_owned(),
        source,
    })?;
    text.push('\n');
    std::fs::write(path, text).map_err(|source| ReportError::Write {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_defaults_and_thresholds() {
        let c: CellDef = serde_json::from_str(
            r#"{"cell":"C15","kind":"saturation","gates":[
                {"metric":"saturation_rps","op":">=","threshold":{"times":3.2,"of":"C12/saturation_rps"}},
                {"metric":"overhead_us.p99","op":"<=","threshold":500}]}"#,
        )
        .unwrap();
        assert_eq!(c.protocol, "h1");
        assert_eq!(c.nodes, 1);
        assert_eq!(c.base_id(), "C15");
        assert!(
            matches!(c.gates[0].threshold, Threshold::Relative { times, .. } if (times - 3.2).abs() < 1e-9)
        );
        assert_eq!(c.gates[1].threshold, Threshold::Value(500.0));
    }
}

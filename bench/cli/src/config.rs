//! Where the harness's files are, and the topology, scenario and cell definitions it reads.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use antirion_bench_report::model::CellDef;
use anyhow::{Context, bail};
use serde::Deserialize;

/// The `bench/` directory and the repository around it.
#[derive(Debug, Clone)]
pub(crate) struct Paths {
    pub(crate) bench: PathBuf,
    pub(crate) repo: PathBuf,
}

impl Paths {
    pub(crate) fn locate(explicit: Option<&Path>) -> anyhow::Result<Self> {
        let candidates: Vec<PathBuf> = explicit
            .map(Path::to_path_buf)
            .into_iter()
            .chain(std::env::var_os("ANTIRION_BENCH_DIR").map(PathBuf::from))
            .chain(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .map(Path::to_path_buf),
            )
            .chain(std::env::current_dir().ok().map(|d| d.join("bench")))
            .chain(std::env::current_dir().ok())
            .collect();
        for c in candidates {
            if c.join("matrix").join("cells.yaml").is_file() {
                let bench = c.canonicalize().unwrap_or(c);
                let repo = bench.parent().map_or_else(|| bench.clone(), Path::to_path_buf);
                return Ok(Self { bench, repo });
            }
        }
        bail!("cannot find the bench directory (matrix/cells.yaml); pass --bench-dir")
    }

    pub(crate) fn corpus(&self) -> PathBuf {
        self.bench.join("corpus").join("data")
    }

    pub(crate) fn profile(&self, name: &str) -> PathBuf {
        self.bench
            .join("profiles")
            .join(format!("{}.yaml", name.to_ascii_lowercase()))
    }
}

pub(crate) fn read_yaml<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    serde_saphyr::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Launch {
    /// The harness starts and stops the process on this machine.
    #[default]
    Local,
    /// The process runs elsewhere; the harness only talks to it.
    External,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct NodeAddr {
    pub(crate) data: SocketAddr,
    pub(crate) admin: SocketAddr,
    /// The agent of the node's machine, when the node runs on another machine.
    #[serde(default)]
    pub(crate) agent: Option<SocketAddr>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GatewayRole {
    #[serde(default)]
    pub(crate) launch: Launch,
    /// The binary, relative to the repository root.
    #[serde(default)]
    pub(crate) bin: Option<String>,
    pub(crate) nodes: Vec<NodeAddr>,
    /// Gateway machines by name, for cells that name one.
    #[serde(default)]
    pub(crate) machines: BTreeMap<String, Vec<NodeAddr>>,
    #[serde(default)]
    pub(crate) nice: i32,
    /// CPU list for `taskset` (Linux).
    #[serde(default)]
    pub(crate) cpus: Option<String>,
    /// The environment variable holding the Admin API token of external nodes.
    #[serde(default)]
    pub(crate) admin_token_env: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MockRole {
    #[serde(default)]
    pub(crate) launch: Launch,
    pub(crate) listen: SocketAddr,
    #[serde(default)]
    pub(crate) workers: Option<usize>,
    #[serde(default)]
    pub(crate) nice: i32,
    #[serde(default)]
    pub(crate) cpus: Option<String>,
    /// The agent of the mock's machine, when the harness starts the mock on another machine.
    #[serde(default)]
    pub(crate) agent: Option<SocketAddr>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GeneratorRole {
    #[serde(default)]
    pub(crate) workers: Option<usize>,
    #[serde(default = "max_connections")]
    pub(crate) max_connections: usize,
    #[serde(default = "h2c_connections")]
    pub(crate) h2c_connections: usize,
    #[serde(default = "h2c_streams")]
    pub(crate) h2c_streams: usize,
}

fn max_connections() -> usize {
    8192
}
fn h2c_connections() -> usize {
    64
}
fn h2c_streams() -> usize {
    256
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SelftestCfg {
    pub(crate) mock_rate: f64,
    pub(crate) mock_p99_us: f64,
    #[serde(default = "five")]
    pub(crate) duration_s: f64,
}

fn five() -> f64 {
    5.0
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct StoreCfg {
    pub(crate) postgres_url: String,
    pub(crate) valkey_url: String,
}

/// A topology file.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Topology {
    pub(crate) name: String,
    pub(crate) reference: bool,
    pub(crate) gateway: GatewayRole,
    pub(crate) mock: MockRole,
    pub(crate) generator: GeneratorRole,
    pub(crate) selftest: SelftestCfg,
    #[serde(default)]
    pub(crate) store: Option<StoreCfg>,
    #[serde(skip)]
    pub(crate) file: PathBuf,
}

impl Topology {
    /// `laptop` names `topologies/laptop.yaml`; anything with a slash or extension is a path.
    pub(crate) fn load(paths: &Paths, name: &str) -> anyhow::Result<Self> {
        let file = if name.contains('/') || name.ends_with(".yaml") {
            PathBuf::from(name)
        } else {
            paths.bench.join("topologies").join(format!("{name}.yaml"))
        };
        let mut t: Topology = read_yaml(&file)?;
        t.file = file;
        if t.gateway.nodes.is_empty() {
            bail!("topology {} lists no gateway node", t.name);
        }
        Ok(t)
    }

    /// The gateway nodes a cell runs on: the named machine's, else the first `count`.
    pub(crate) fn nodes(&self, machine: Option<&str>, count: usize) -> anyhow::Result<Vec<NodeAddr>> {
        let all = match machine {
            Some(m) => self
                .gateway
                .machines
                .get(m)
                .with_context(|| format!("topology {} has no gateway machine {m}", self.name))?,
            None => &self.gateway.nodes,
        };
        if all.len() < count {
            bail!(
                "topology {} has {} gateway nodes; the cell needs {count}",
                self.name,
                all.len()
            );
        }
        Ok(all.iter().take(count).copied().collect())
    }
}

/// A scenario of `matrix/scenarios.yaml`.
#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct ScenarioDef {
    #[serde(default)]
    pub(crate) entry: Option<String>,
    #[serde(default)]
    pub(crate) stream: bool,
    #[serde(default)]
    pub(crate) key: Option<String>,
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) expect: Option<u16>,
    #[serde(default = "yes")]
    pub(crate) direct: bool,
    #[serde(default)]
    pub(crate) session: Option<String>,
    #[serde(default)]
    pub(crate) models: Option<String>,
    #[serde(default)]
    pub(crate) mix: Option<BTreeMap<String, u32>>,
}

fn yes() -> bool {
    true
}

pub(crate) fn scenarios(paths: &Paths) -> anyhow::Result<BTreeMap<String, ScenarioDef>> {
    read_yaml(&paths.bench.join("matrix").join("scenarios.yaml"))
}

pub(crate) fn cells(paths: &Paths) -> anyhow::Result<Vec<CellDef>> {
    read_yaml(&paths.bench.join("matrix").join("cells.yaml"))
}

/// The cells a suite runs, in matrix order.
pub(crate) fn suite(all: &[CellDef], suite: &str) -> Vec<CellDef> {
    all.iter()
        .filter(|c| {
            if c.suites.is_empty() {
                suite == "release" || (suite == "compare" && c.compare)
            } else {
                c.suites.iter().any(|s| s == suite) || (suite == "compare" && c.compare)
            }
        })
        .cloned()
        .collect()
}

/// `500ms`, `5s`, `2m`, `1h`, or plain seconds.
pub(crate) fn parse_duration(s: &str) -> anyhow::Result<std::time::Duration> {
    let s = s.trim();
    let (num, unit) = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map_or((s, "s"), |i| s.split_at(i));
    let v: f64 = num.parse().with_context(|| format!("bad duration {s:?}"))?;
    let secs = match unit {
        "ms" => v / 1000.0,
        "s" => v,
        "m" => v * 60.0,
        "h" => v * 3600.0,
        _ => bail!("bad duration unit in {s:?}"),
    };
    Ok(std::time::Duration::from_secs_f64(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Paths {
        Paths::locate(None).unwrap()
    }

    #[test]
    fn every_topology_profile_and_cell_parses() {
        let p = paths();
        for t in ["laptop", "local", "ref-4", "ref-16", "t3-published"] {
            Topology::load(&p, t).unwrap();
        }
        let cells = cells(&p).unwrap();
        let scen = scenarios(&p).unwrap();
        let mut ids = std::collections::BTreeSet::new();
        for c in &cells {
            assert!(ids.insert(c.cell.clone()), "duplicate {}", c.cell);
            if let Some(s) = &c.scenario {
                assert!(scen.contains_key(s), "{}: unknown scenario {s}", c.cell);
            }
            if let Some(pr) = &c.profile {
                assert!(p.profile(pr).is_file(), "{}: no profile {pr}", c.cell);
            }
            if let Some(m) = &c.mock_profile {
                m.parse::<antirion_bench_mock::Profile>().unwrap();
            }
            if let Some(a) = &c.alternate_with {
                assert!(cells.iter().any(|o| &o.cell == a), "{}: no cell {a}", c.cell);
            }
        }
        for n in 1..=33 {
            let id = format!("C{n}");
            assert!(cells.iter().any(|c| c.base_id() == id), "{id} is missing");
        }
        assert_eq!(suite(&cells, "nightly").len(), 3);
        assert!(suite(&cells, "compare").iter().all(|c| c.compare));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms").unwrap().as_millis(), 500);
        assert_eq!(parse_duration("2m").unwrap().as_secs(), 120);
        assert_eq!(parse_duration("7").unwrap().as_secs(), 7);
        assert!(parse_duration("3x").is_err());
    }
}

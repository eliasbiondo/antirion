//! Other gateways under test: where they listen, how a request authenticates, the model names that
//! reach the mock under each latency profile, which processes are theirs, and how to start them. A targets file
//! lists several targets (the harness's own gateway among them) for an interleaved run.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use antirion_bench_mock::Profile;
use anyhow::{Context, bail};
use serde::Deserialize;

use crate::config::read_yaml;
use crate::ctx::Target;
use crate::hosts::{self, Host, Pid};

/// The header requests carry the key in by default; it gets the `Bearer` scheme.
pub(crate) const DEFAULT_AUTH_HEADER: &str = "authorization";

/// A gateway the harness does not configure.
#[derive(Debug, Clone)]
pub(crate) struct External {
    pub(crate) name: String,
    pub(crate) nodes: Vec<SocketAddr>,
    pub(crate) key: String,
    /// The header carrying the key: `authorization` sends `Bearer <key>`, any other the key alone.
    pub(crate) auth_header: String,
    /// The target's name for a corpus model, by `<model>@<profile>` or by `<model>` for every
    /// profile without its own entry.
    pub(crate) models: BTreeMap<String, String>,
    /// Processes on this machine whose memory and CPU are the target's.
    pub(crate) pids: Vec<u32>,
    /// The agent of the target's machine, when it runs on another one: the harness starts it, finds
    /// its processes and samples them there.
    pub(crate) agent: Option<SocketAddr>,
    /// A pattern of the target's command lines (`pgrep -f`), resolved each time the target comes up.
    pub(crate) process: Option<String>,
    pub(crate) version: Option<String>,
    /// Started for every repetition, like the harness's own gateway, when given.
    pub(crate) start: Option<StartCmd>,
    /// The status the target answers a scenario with when it is not the scenario's own (a rejection
    /// the target signals with another code), by scenario name.
    pub(crate) statuses: BTreeMap<String, u16>,
}

/// How the harness starts an external target.
#[derive(Debug, Clone)]
pub(crate) struct StartCmd {
    pub(crate) command: Vec<String>,
    /// Added to the harness's own environment.
    pub(crate) env: Vec<(String, String)>,
    pub(crate) cwd: Option<PathBuf>,
    /// The path that answers 200 once the target serves.
    pub(crate) ready: String,
    pub(crate) ready_timeout: Duration,
    /// Waited after the ready probe, for worker processes that start behind the first.
    pub(crate) settle: Duration,
}

impl External {
    /// The target's name for corpus model `model` under latency profile `mock`, when it renames it.
    pub(crate) fn model_for(&self, model: &str, mock: Profile) -> Option<&str> {
        self.models
            .get(&format!("{model}@{}", mock.name()))
            .or_else(|| self.models.get(model))
            .map(String::as_str)
    }
}

/// `host:port`, with or without `http://`.
pub(crate) fn parse_node(u: &str) -> anyhow::Result<SocketAddr> {
    let hostport = u.trim_start_matches("http://").trim_end_matches('/');
    hostport
        .parse::<SocketAddr>()
        .with_context(|| format!("{u} is not host:port"))
}

/// `<model>` or `<model>@<profile>`, with the profile's name normalised.
fn model_key(from: &str) -> anyhow::Result<String> {
    Ok(match from.split_once('@') {
        Some((m, p)) => format!("{m}@{}", p.parse::<Profile>()?.name()),
        None => from.to_owned(),
    })
}

/// `TO` (the chat model under every profile), `FROM=TO` or `FROM@PROFILE=TO`, as `--target-model`
/// takes them.
pub(crate) fn parse_models(specs: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for s in specs {
        let (from, to) = match s.split_once('=') {
            Some((f, t)) => (model_key(f)?, t),
            None => (antirion_bench_corpus::entries::CHAT_MODEL.to_owned(), s.as_str()),
        };
        if to.is_empty() || from.is_empty() {
            bail!("--target-model {s:?} names no model");
        }
        out.insert(from, to.to_owned());
    }
    Ok(out)
}

/// One target of a targets file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    /// `antirion` (the harness starts its own gateway) or `external` (default).
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    nodes: Vec<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    auth_header: Option<String>,
    /// The target's name for each corpus model: keys `<model>` or `<model>@<profile>`.
    #[serde(default)]
    models: BTreeMap<String, String>,
    #[serde(default)]
    process: Option<String>,
    #[serde(default)]
    version: Option<String>,
    /// Program and arguments; a relative program path is relative to the file.
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    ready: Option<String>,
    #[serde(default)]
    ready_timeout_s: Option<f64>,
    #[serde(default)]
    settle_s: Option<f64>,
    /// The agent (`host:port`) of the machine the target runs on.
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    statuses: BTreeMap<String, u16>,
}

fn relative_to(base: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// The targets of a file, in its order (the order of every interleaved round).
pub(crate) fn load(file: &Path) -> anyhow::Result<Vec<Target>> {
    let entries: Vec<Entry> = read_yaml(file)?;
    let base = file
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let base = base.canonicalize().unwrap_or(base);
    let mut names = BTreeSet::new();
    let mut out = Vec::new();
    for e in entries {
        if !names.insert(e.name.clone()) {
            bail!("{}: target {} is listed twice", file.display(), e.name);
        }
        if e.name.is_empty() || e.name.contains(['/', '\\']) || e.name.starts_with('.') {
            bail!("{}: {:?} cannot name a result directory", file.display(), e.name);
        }
        match e.kind.as_deref().unwrap_or("external") {
            "antirion" => {
                if e.name != "antirion" {
                    bail!("{}: the harness's own gateway is named antirion", file.display());
                }
                out.push(Target::Antirion);
            }
            "external" => out.push(Target::External(Box::new(external(e, &base)?))),
            other => bail!("{}: unknown kind {other} (antirion, external)", file.display()),
        }
    }
    if out.is_empty() {
        bail!("{} lists no target", file.display());
    }
    Ok(out)
}

fn external(e: Entry, base: &Path) -> anyhow::Result<External> {
    let nodes = e
        .nodes
        .iter()
        .map(|u| parse_node(u))
        .collect::<anyhow::Result<Vec<_>>>()?;
    if nodes.is_empty() {
        bail!("target {} lists no nodes", e.name);
    }
    let models = e
        .models
        .into_iter()
        .map(|(from, to)| Ok((model_key(&from)?, to)))
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let start = if e.command.is_empty() {
        None
    } else {
        let mut command = e.command;
        if let Some(program) = command.first_mut()
            && program.contains('/')
        {
            *program = relative_to(base, program).display().to_string();
        }
        Some(StartCmd {
            command,
            env: e.env.into_iter().collect(),
            cwd: Some(base.to_path_buf()),
            ready: e.ready.unwrap_or_else(|| "/".to_owned()),
            ready_timeout: Duration::from_secs_f64(e.ready_timeout_s.unwrap_or(120.0)),
            settle: Duration::from_secs_f64(e.settle_s.unwrap_or(1.0)),
        })
    };
    let agent = e.agent.as_deref().map(parse_node).transpose()?;
    Ok(External {
        agent,
        key: e.key.with_context(|| format!("target {} has no key", e.name))?,
        name: e.name,
        nodes,
        auth_header: e.auth_header.unwrap_or_else(|| DEFAULT_AUTH_HEADER.to_owned()),
        models,
        pids: Vec::new(),
        process: e.process,
        version: e.version,
        start,
        statuses: e.statuses,
    })
}

async fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Processes whose command line matches `pattern`, except the harness itself.
pub(crate) async fn matching(pattern: &str) -> Vec<u32> {
    let me = std::process::id();
    output("pgrep", &["-f", pattern])
        .await
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .filter(|&p| p != me)
        .collect()
}

/// `roots` and every process below them.
pub(crate) async fn with_descendants(roots: &[u32]) -> Vec<u32> {
    let table = output("ps", &["-A", "-o", "pid=,ppid="])
        .await
        .unwrap_or_default();
    descend(roots, &parse_ps(&table))
}

fn parse_ps(table: &str) -> Vec<(u32, u32)> {
    table
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
        })
        .collect()
}

fn descend(roots: &[u32], pairs: &[(u32, u32)]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut queue: Vec<u32> = roots.to_vec();
    while let Some(p) = queue.pop() {
        if !seen.insert(p) {
            continue;
        }
        out.push(p);
        queue.extend(pairs.iter().filter(|(_, parent)| *parent == p).map(|(c, _)| *c));
    }
    out
}

/// The target's processes now: its given pids, the one the harness started and those its pattern
/// matches, with their children.
pub(crate) async fn resolve_pids(e: &External, started: Option<Pid>) -> Vec<Pid> {
    let host = Host::of(e.agent);
    let mut out: Vec<Pid> = Vec::new();
    let local: Vec<u32> = e.pids.clone();
    if !local.is_empty() {
        out.extend(with_descendants(&local).await.into_iter().map(Pid::local));
    }
    let mut roots: Vec<u32> = started.map(|p| p.pid).into_iter().collect();
    if let Some(p) = &e.process {
        roots.extend(hosts::matching(host, p).await);
    }
    if !roots.is_empty() {
        out.extend(
            hosts::with_descendants(host, &roots)
                .await
                .into_iter()
                .map(|pid| Pid { host, pid }),
        );
    }
    let mut seen = BTreeSet::new();
    out.retain(|p| seen.insert((p.host, p.pid)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_by_profile() {
        let m = parse_models(&["bench-chat@ls=b/x".into(), "a/x".into(), "bench-embed=e/x".into()]).unwrap();
        let e = External {
            name: "x".into(),
            nodes: Vec::new(),
            key: "k".into(),
            auth_header: DEFAULT_AUTH_HEADER.into(),
            models: m,
            pids: Vec::new(),
            agent: None,
            process: None,
            version: None,
            start: None,
            statuses: BTreeMap::new(),
        };
        assert_eq!(e.model_for("bench-chat", Profile::Ls), Some("b/x"));
        assert_eq!(e.model_for("bench-chat", Profile::L0), Some("a/x"));
        assert_eq!(e.model_for("bench-embed", Profile::Ls), Some("e/x"));
        assert_eq!(e.model_for("bench-messages", Profile::L0), None);
        assert!(parse_models(&["bench-chat=".into()]).is_err());
        assert!(parse_models(&["bench-chat@L7=x".into()]).is_err());
    }

    #[test]
    fn descendants_of_a_tree() {
        let pairs = parse_ps(" 10 1\n 11 10\n 12 11\n 13 1\n x y\n");
        let mut d = descend(&[10], &pairs);
        d.sort_unstable();
        assert_eq!(d, vec![10, 11, 12]);
    }

    #[test]
    fn a_targets_file() {
        let dir = std::env::temp_dir().join(format!("bench-targets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("targets.yaml");
        std::fs::write(
            &file,
            "- {name: antirion, kind: antirion}\n\
             - {name: other, nodes: [\"127.0.0.1:9\"], key: k, auth_header: x-key,\n   \
             models: {bench-chat: m, bench-chat@LS: m-ls}, command: [./start.sh, --fast], ready: /health}\n",
        )
        .unwrap();
        let t = load(&file).unwrap();
        assert!(matches!(t[0], Target::Antirion));
        let Target::External(e) = &t[1] else {
            panic!("not external")
        };
        assert_eq!(e.model_for("bench-chat", Profile::Ls), Some("m-ls"));
        assert_eq!(e.model_for("bench-chat", Profile::L0), Some("m"));
        let start = e.start.as_ref().unwrap();
        assert!(start.command[0].ends_with("/start.sh"));
        assert_eq!(start.ready, "/health");
        std::fs::write(&file, "- {name: a, kind: antirion}\n").unwrap();
        assert!(load(&file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

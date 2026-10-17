//! Where the harness's processes run: on this machine, or on another one through its agent
//! (`antirion-bench agent`), which starts, samples and stops them there. Paths given for another
//! machine are paths on that machine.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use antirion_bench_loadgen::fetch;
use anyhow::{Context, bail};
use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::procs::{self, Proc, Spawn};
use crate::sampler::{self, Reading};
use crate::targets;

/// The environment variable holding the token agents and the harness share.
pub(crate) const TOKEN_ENV: &str = "ANTIRION_BENCH_AGENT_TOKEN";

/// A machine processes run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Host {
    Local,
    Agent(SocketAddr),
}

impl Host {
    pub(crate) fn of(agent: Option<SocketAddr>) -> Self {
        agent.map_or(Host::Local, Host::Agent)
    }
}

/// A process on a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pid {
    pub(crate) host: Host,
    pub(crate) pid: u32,
}

impl Pid {
    pub(crate) fn local(pid: u32) -> Self {
        Self {
            host: Host::Local,
            pid,
        }
    }
}

fn token() -> String {
    std::env::var(TOKEN_ENV).unwrap_or_default()
}

async fn call<T: Serialize, R: DeserializeOwned>(
    agent: SocketAddr,
    path: &str,
    body: &T,
    timeout: Duration,
) -> anyhow::Result<R> {
    let req = Request::post(path)
        .header("host", agent.to_string())
        .header("authorization", format!("Bearer {}", token()))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(serde_json::to_vec(body)?)))?;
    let (status, _, text) = fetch(agent, req, timeout)
        .await
        .with_context(|| format!("agent {agent}{path}"))?;
    if !status.is_success() {
        bail!(
            "agent {agent}{path} answered {status}: {}",
            String::from_utf8_lossy(&text)
        );
    }
    serde_json::from_slice(&text).with_context(|| format!("agent {agent}{path}: bad answer"))
}

const QUICK: Duration = Duration::from_secs(10);

/// The agent's wire format.
pub(crate) mod wire {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub(crate) struct SpawnReq {
        pub(crate) name: String,
        pub(crate) program: String,
        pub(crate) args: Vec<String>,
        pub(crate) nice: i32,
        pub(crate) cpus: Option<String>,
        pub(crate) env: Option<Vec<(String, String)>>,
        pub(crate) add_env: Vec<(String, String)>,
        pub(crate) cwd: Option<String>,
        pub(crate) log: String,
        /// Directories removed and created again before the start.
        pub(crate) clean: Vec<String>,
        /// Text files written before the start.
        pub(crate) files: Vec<(String, String)>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub(crate) struct SpawnResp {
        pub(crate) id: u64,
        pub(crate) pid: u32,
        pub(crate) argv: Vec<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub(crate) struct Id {
        pub(crate) id: u64,
        #[serde(default)]
        pub(crate) grace_ms: u64,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub(crate) struct Pids {
        pub(crate) pids: Vec<u32>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub(crate) struct Pattern {
        pub(crate) pattern: String,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    pub(crate) struct Usage {
        pub(crate) rss_bytes: u64,
        pub(crate) cpu_s: f64,
        /// CPU of the processes' own cgroup, when each runs in one (a container).
        pub(crate) cgroup_cpu_s: Option<f64>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub(crate) struct InventoryReq {
        pub(crate) machine: String,
        pub(crate) mock: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub(crate) struct Inventory {
        pub(crate) doc: serde_json::Value,
        pub(crate) sysctl: String,
        pub(crate) ulimit: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub(crate) struct Info {
        pub(crate) work_dir: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub(crate) struct Exited {
        pub(crate) exited: Option<String>,
    }
}

/// Readings of local processes, as the agent answers them for its own.
pub(crate) async fn local_usage(pids: &[u32]) -> Option<wire::Usage> {
    let r = sampler::read_all(pids).await?;
    let cgroup = if !pids.is_empty() && pids.iter().all(|&p| sampler::own_cgroup(p)) {
        pids.iter().map(|&p| sampler::cgroup_cpu_s(p)).sum()
    } else {
        None
    };
    Some(wire::Usage {
        rss_bytes: r.rss_bytes,
        cpu_s: r.cpu_s,
        cgroup_cpu_s: cgroup,
    })
}

/// Open descriptors of local processes.
pub(crate) async fn local_fds(pids: &[u32]) -> Option<u64> {
    let mut total = 0;
    for &p in pids {
        total += sampler::fds(p).await?;
    }
    Some(total)
}

fn by_host(pids: &[Pid]) -> BTreeMap<Host, Vec<u32>> {
    let mut out: BTreeMap<Host, Vec<u32>> = BTreeMap::new();
    for p in pids {
        out.entry(p.host).or_default().push(p.pid);
    }
    out
}

async fn usage_on(host: Host, pids: &[u32]) -> Option<wire::Usage> {
    match host {
        Host::Local => local_usage(pids).await,
        Host::Agent(a) => {
            call::<_, Option<wire::Usage>>(a, "/read", &wire::Pids { pids: pids.to_vec() }, QUICK)
                .await
                .ok()
                .flatten()
        }
    }
}

/// Summed memory and CPU of `pids`; `None` when any is unreadable. CPU comes from the processes'
/// own cgroups when every one has its own (the second value says so).
pub(crate) async fn usage(pids: &[Pid]) -> Option<(Reading, Option<f64>)> {
    let mut total = Reading::default();
    let mut cgroup = Some(0.0);
    for (host, list) in by_host(pids) {
        let u = usage_on(host, &list).await?;
        total.rss_bytes += u.rss_bytes;
        total.cpu_s += u.cpu_s;
        cgroup = match (cgroup, u.cgroup_cpu_s) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        };
    }
    Some((total, cgroup.filter(|_| !pids.is_empty())))
}

/// Summed memory and CPU of `pids`.
pub(crate) async fn read_all(pids: &[Pid]) -> Option<Reading> {
    usage(pids).await.map(|(r, _)| r)
}

/// Open descriptors summed over `pids`.
pub(crate) async fn fds(pids: &[Pid]) -> Option<u64> {
    let mut total = 0;
    for (host, list) in by_host(pids) {
        total += match host {
            Host::Local => local_fds(&list).await?,
            Host::Agent(a) => call::<_, Option<u64>>(a, "/fds", &wire::Pids { pids: list }, QUICK)
                .await
                .ok()
                .flatten()?,
        };
    }
    Some(total)
}

/// `roots` on `host` and every process below them.
pub(crate) async fn with_descendants(host: Host, roots: &[u32]) -> Vec<u32> {
    match host {
        Host::Local => targets::with_descendants(roots).await,
        Host::Agent(a) => call(a, "/descendants", &wire::Pids { pids: roots.to_vec() }, QUICK)
            .await
            .unwrap_or_default(),
    }
}

/// Processes on `host` whose command line matches `pattern`.
pub(crate) async fn matching(host: Host, pattern: &str) -> Vec<u32> {
    match host {
        Host::Local => targets::matching(pattern).await,
        Host::Agent(a) => call(
            a,
            "/match",
            &wire::Pattern {
                pattern: pattern.to_owned(),
            },
            QUICK,
        )
        .await
        .unwrap_or_default(),
    }
}

/// Kills whichever of `pids` still run.
pub(crate) async fn kill_all(pids: &[Pid]) {
    for (host, list) in by_host(pids) {
        match host {
            Host::Local => {
                procs::kill_all(&list).await;
            }
            Host::Agent(a) => {
                let _ = call::<_, Value>(a, "/kill", &wire::Pids { pids: list }, QUICK).await;
            }
        }
    }
}

/// The directory processes on `host` keep their files in (`local` for this machine).
pub(crate) async fn work_dir(host: Host, local: &Path) -> anyhow::Result<PathBuf> {
    match host {
        Host::Local => Ok(local.to_path_buf()),
        Host::Agent(a) => {
            let info: wire::Info = call(a, "/info", &Value::Null, QUICK).await?;
            Ok(PathBuf::from(info.work_dir))
        }
    }
}

/// The inventory of the agent's machine.
pub(crate) async fn inventory(
    agent: SocketAddr,
    machine: &str,
    mock: SocketAddr,
) -> anyhow::Result<wire::Inventory> {
    call(
        agent,
        "/inventory",
        &wire::InventoryReq {
            machine: machine.to_owned(),
            mock: Some(mock.to_string()),
        },
        Duration::from_secs(60),
    )
    .await
}

/// A process started on another machine.
#[derive(Debug)]
pub(crate) struct Remote {
    agent: SocketAddr,
    id: u64,
    pid: u32,
    argv: Vec<String>,
}

/// A process the harness started, here or on another machine.
#[derive(Debug)]
pub(crate) enum Running {
    Local(Proc),
    Remote(Remote),
}

impl Running {
    pub(crate) fn pid(&self) -> Pid {
        match self {
            Running::Local(p) => Pid::local(p.pid),
            Running::Remote(r) => Pid {
                host: Host::Agent(r.agent),
                pid: r.pid,
            },
        }
    }

    pub(crate) fn argv(&self) -> &[String] {
        match self {
            Running::Local(p) => &p.argv,
            Running::Remote(r) => &r.argv,
        }
    }

    /// Stops the process and the processes below it, as [`Proc::stop`] does, on its machine.
    pub(crate) async fn stop(self, grace: Duration) {
        match self {
            Running::Local(p) => {
                p.stop(grace).await;
            }
            Running::Remote(r) => {
                let _ = call::<_, Value>(
                    r.agent,
                    "/stop",
                    &wire::Id {
                        id: r.id,
                        grace_ms: u64::try_from(grace.as_millis()).unwrap_or(u64::MAX),
                    },
                    grace + procs::KILL_WAIT + QUICK,
                )
                .await;
            }
        }
    }

    /// Whether it has exited, with its log's tail when it has.
    pub(crate) async fn exited(&mut self) -> Option<String> {
        match self {
            Running::Local(p) => p.exited(),
            Running::Remote(r) => match call::<_, wire::Exited>(
                r.agent,
                "/status",
                &wire::Id {
                    id: r.id,
                    grace_ms: 0,
                },
                QUICK,
            )
            .await
            {
                Ok(e) => e.exited,
                Err(e) => Some(format!("{e:#}")),
            },
        }
    }
}

/// Starts `s` on `host` after removing and creating `clean` and writing `files` there.
pub(crate) async fn spawn(
    host: Host,
    s: &Spawn<'_>,
    clean: &[PathBuf],
    files: &[(PathBuf, String)],
) -> anyhow::Result<Running> {
    match host {
        Host::Local => {
            for d in clean {
                let _ = std::fs::remove_dir_all(d);
                std::fs::create_dir_all(d)?;
            }
            for (path, text) in files {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, text)?;
            }
            Ok(Running::Local(procs::spawn(s)?))
        }
        Host::Agent(a) => {
            let req = wire::SpawnReq {
                name: s.name.to_owned(),
                program: s.program.display().to_string(),
                args: s.args.clone(),
                nice: s.nice,
                cpus: s.cpus.map(str::to_owned),
                env: s.env.map(<[(String, String)]>::to_vec),
                add_env: s.add_env.to_vec(),
                cwd: s.cwd.map(|c| c.display().to_string()),
                log: s.log.display().to_string(),
                clean: clean.iter().map(|d| d.display().to_string()).collect(),
                files: files
                    .iter()
                    .map(|(p, t)| (p.display().to_string(), t.clone()))
                    .collect(),
            };
            let r: wire::SpawnResp = call(a, "/spawn", &req, Duration::from_secs(60)).await?;
            Ok(Running::Remote(Remote {
                agent: a,
                id: r.id,
                pid: r.pid,
                argv: r.argv,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pids_group_by_host() {
        let a: SocketAddr = "10.0.0.1:1".parse().unwrap();
        let pids = [
            Pid::local(1),
            Pid {
                host: Host::Agent(a),
                pid: 2,
            },
            Pid::local(3),
        ];
        let g = by_host(&pids);
        assert_eq!(g[&Host::Local], vec![1, 3]);
        assert_eq!(g[&Host::Agent(a)], vec![2]);
    }

    #[tokio::test]
    async fn reads_this_process_through_the_local_host() {
        let me = [Pid::local(std::process::id())];
        let (r, _) = usage(&me).await.unwrap();
        assert!(r.rss_bytes > 0);
        assert!(fds(&me).await.unwrap() >= 3);
        assert!(usage(&[]).await.unwrap().1.is_none());
    }
}

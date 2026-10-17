//! Processes the harness starts: the mock and the gateway nodes, with their priority and CPU set,
//! their logs, and a stop that lets them drain and returns once they and their children are gone.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use antirion_bench_loadgen::fetch;
use anyhow::{Context, bail};
use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use tokio::net::TcpStream;
use tokio::process::{Child, Command};

use crate::hosts::Running;
use crate::targets;

/// A started process.
#[derive(Debug)]
pub(crate) struct Proc {
    pub(crate) name: String,
    pub(crate) pid: u32,
    pub(crate) argv: Vec<String>,
    pub(crate) log: PathBuf,
    child: Child,
}

/// How to start a process.
#[derive(Debug, Clone, Default)]
pub(crate) struct Spawn<'a> {
    pub(crate) name: &'a str,
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) nice: i32,
    pub(crate) cpus: Option<&'a str>,
    /// Replaces the environment, except `PATH`, `HOME` and `TMPDIR`.
    pub(crate) env: Option<&'a [(String, String)]>,
    /// Added to the inherited environment (when `env` does not replace it).
    pub(crate) add_env: &'a [(String, String)],
    pub(crate) cwd: Option<&'a Path>,
    pub(crate) log: PathBuf,
}

/// Starts `s`; `nice` and `taskset` exec the program, so the pid is the program's own.
pub(crate) fn spawn(s: &Spawn<'_>) -> anyhow::Result<Proc> {
    let mut argv: Vec<String> = Vec::new();
    if let Some(cpus) = s.cpus.filter(|_| cfg!(target_os = "linux")) {
        argv.extend(["taskset".to_owned(), "-c".to_owned(), cpus.to_owned()]);
    }
    if s.nice != 0 {
        argv.extend(["nice".to_owned(), "-n".to_owned(), s.nice.to_string()]);
    }
    argv.push(s.program.display().to_string());
    argv.extend(s.args.iter().cloned());
    let log = std::fs::File::create(&s.log).with_context(|| format!("cannot create {}", s.log.display()))?;
    let err = log.try_clone()?;
    let mut cmd = Command::new(argv.first().context("empty command")?);
    cmd.args(argv.iter().skip(1))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err))
        .kill_on_drop(true);
    if let Some(env) = s.env {
        cmd.env_clear();
        for k in ["PATH", "HOME", "TMPDIR"] {
            if let Some(v) = std::env::var_os(k) {
                cmd.env(k, v);
            }
        }
        cmd.envs(env.iter().map(|(k, v)| (k, v)));
    }
    cmd.envs(s.add_env.iter().map(|(k, v)| (k, v)));
    if let Some(dir) = s.cwd {
        cmd.current_dir(dir);
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("cannot start {} ({})", s.name, s.program.display()))?;
    let pid = child.id().context("the process exited at once")?;
    Ok(Proc {
        name: s.name.to_owned(),
        pid,
        argv,
        log: s.log.clone(),
        child,
    })
}

/// How long a stop waits for killed processes to go away.
pub(crate) const KILL_WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);

impl Proc {
    /// Stops the process and every process below it: SIGTERM to the process, and to the processes
    /// it leaves behind once it exits, then SIGKILL to whatever still runs at `grace` (or `grace`
    /// after the process itself exited). Returns once all of them have exited, or after a further
    /// [`KILL_WAIT`]; the pids still running then (normally none).
    pub(crate) async fn stop(mut self, grace: Duration) -> Vec<u32> {
        let mut family: BTreeSet<u32> = targets::with_descendants(&[self.pid]).await.into_iter().collect();
        family.remove(&self.pid);
        signal("TERM", &[self.pid]).await;
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if tokio::time::timeout(POLL * 2, self.child.wait()).await.is_ok() {
                break;
            }
            family.extend(targets::with_descendants(&[self.pid]).await);
            family.remove(&self.pid);
        }
        let family: Vec<u32> = family.into_iter().collect();
        let mut left = running(&family).await;
        let root_done = self.child.try_wait().ok().flatten().is_some();
        if root_done && !left.is_empty() {
            signal("TERM", &left).await;
            left = wait_gone(&left, deadline.max(Instant::now() + POLL * 40)).await;
        }
        if root_done && left.is_empty() {
            return left;
        }
        let _ = self.child.start_kill();
        signal("KILL", &left).await;
        let until = Instant::now() + KILL_WAIT;
        let root_gone = tokio::time::timeout(KILL_WAIT, self.child.wait())
            .await
            .is_ok_and(|r| r.is_ok());
        let mut left = wait_gone(&left, until).await;
        if !root_gone {
            left.push(self.pid);
        }
        left
    }

    /// Whether it has exited, with its log's tail when it has.
    pub(crate) fn exited(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(format!("{} exited ({status}): {}", self.name, tail(&self.log))),
            _ => None,
        }
    }
}

async fn signal(name: &str, pids: &[u32]) {
    if pids.is_empty() {
        return;
    }
    let mut args = vec![format!("-{name}")];
    args.extend(pids.iter().map(u32::to_string));
    let _ = Command::new("kill")
        .args(&args)
        .stderr(Stdio::null())
        .status()
        .await;
}

/// Which of `pids` still run; an exited process waiting to be reaped does not.
async fn running(pids: &[u32]) -> Vec<u32> {
    if pids.is_empty() {
        return Vec::new();
    }
    let list: Vec<String> = pids.iter().map(u32::to_string).collect();
    let out = Command::new("ps")
        .args(["-o", "pid=,stat=", "-p", &list.join(",")])
        .stderr(Stdio::null())
        .output()
        .await
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    live_pids(&out, pids)
}

fn live_pids(ps: &str, asked: &[u32]) -> Vec<u32> {
    ps.lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let pid: u32 = parts.next()?.parse().ok()?;
            let stat = parts.next().unwrap_or("");
            (!stat.starts_with('Z') && asked.contains(&pid)).then_some(pid)
        })
        .collect()
}

/// Polls until none of `pids` runs or `until`; those still running.
async fn wait_gone(pids: &[u32], until: Instant) -> Vec<u32> {
    let mut left = running(pids).await;
    while !left.is_empty() && Instant::now() < until {
        tokio::time::sleep(POLL).await;
        left = running(&left).await;
    }
    left
}

/// Kills whichever of `pids` still run (workers a stopped parent left behind) and waits, up to
/// [`KILL_WAIT`], until they have exited; those still running.
pub(crate) async fn kill_all(pids: &[u32]) -> Vec<u32> {
    let alive = running(pids).await;
    signal("KILL", &alive).await;
    wait_gone(&alive, Instant::now() + KILL_WAIT).await
}

/// Waits until nothing accepts connections at `addrs` (a stopped process's listeners are closed),
/// for up to `limit`; the first address still accepting then.
pub(crate) async fn wait_free(addrs: &[SocketAddr], limit: Duration) -> Result<(), SocketAddr> {
    let until = Instant::now() + limit;
    for &addr in addrs {
        loop {
            // A connection whose local address is the target's own connected to itself (Linux
            // picks the port as the ephemeral one when the range includes it): nothing listens.
            let open = match tokio::time::timeout(Duration::from_millis(250), TcpStream::connect(addr)).await
            {
                Ok(Ok(stream)) => stream.local_addr().ok() != Some(addr),
                _ => false,
            };
            if !open {
                break;
            }
            if Instant::now() >= until {
                return Err(addr);
            }
            tokio::time::sleep(POLL).await;
        }
    }
    Ok(())
}

/// The last lines of a log.
pub(crate) fn tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(5)..].join(" | ")
}

/// GET `path` on `addr`; the status and body.
pub(crate) async fn get(addr: SocketAddr, path: &str, token: Option<&str>) -> anyhow::Result<(u16, Bytes)> {
    let mut b = Request::get(path).header("host", addr.to_string());
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    let (status, _, body) = fetch(addr, b.body(Full::new(Bytes::new()))?, Duration::from_secs(10)).await?;
    Ok((status.as_u16(), body))
}

/// Polls `path` until it answers 200; the time it took.
pub(crate) async fn wait_ok(
    addr: SocketAddr,
    path: &str,
    limit: Duration,
    proc: Option<&mut Running>,
) -> anyhow::Result<Duration> {
    let start = Instant::now();
    let mut proc = proc;
    let mut checked = Instant::now();
    loop {
        let req = Request::get(path)
            .header("host", addr.to_string())
            .body(Full::new(Bytes::new()))?;
        if let Ok((s, _, _)) = fetch(addr, req, Duration::from_millis(500)).await
            && s.as_u16() == 200
        {
            return Ok(start.elapsed());
        }
        if checked.elapsed() >= Duration::from_millis(100) {
            checked = Instant::now();
            if let Some(p) = proc.as_deref_mut()
                && let Some(why) = p.exited().await
            {
                bail!("{why}");
            }
        }
        if start.elapsed() > limit {
            bail!("{addr}{path} did not answer 200 within {limit:?}");
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(tag: &str, script: &str) -> Proc {
        let dir = std::env::temp_dir().join(format!("bench-procs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        spawn(&Spawn {
            name: "tree",
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), script.into()],
            log: dir.join(format!("{tag}.log")),
            ..Spawn::default()
        })
        .unwrap()
    }

    async fn children(p: &Proc, want: usize) -> Vec<u32> {
        for _ in 0..100 {
            let mut d = targets::with_descendants(&[p.pid]).await;
            d.retain(|&c| c != p.pid);
            if d.len() >= want {
                return d;
            }
            tokio::time::sleep(POLL).await;
        }
        panic!("the children of {} did not start", p.pid)
    }

    #[tokio::test]
    async fn a_stop_waits_for_the_children_a_parent_leaves_behind() {
        let p = start("orphans", "sleep 30 & sleep 30 & wait");
        let kids = children(&p, 2).await;
        let began = std::time::Instant::now();
        let left = p.stop(Duration::from_secs(10)).await;
        assert!(left.is_empty(), "{left:?}");
        assert!(running(&kids).await.is_empty());
        assert!(began.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn what_ignores_sigterm_is_killed_at_the_grace() {
        let p = start("deaf", "trap '' TERM; sleep 30 & wait; sleep 30");
        let mut all = children(&p, 1).await;
        all.push(p.pid);
        let began = std::time::Instant::now();
        let left = p.stop(Duration::from_millis(400)).await;
        assert!(left.is_empty(), "{left:?}");
        assert!(running(&all).await.is_empty());
        assert!(began.elapsed() >= Duration::from_millis(400));
        assert!(began.elapsed() < Duration::from_millis(400) + KILL_WAIT);
    }

    #[tokio::test]
    async fn the_next_start_waits_for_the_port() {
        let held = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = held.local_addr().unwrap();
        assert_eq!(wait_free(&[addr], Duration::from_millis(300)).await, Err(addr));
        let began = std::time::Instant::now();
        let release = async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            drop(held);
        };
        let addrs = [addr];
        let ((), freed) = tokio::join!(release, wait_free(&addrs, Duration::from_secs(5)));
        assert_eq!(freed, Ok(()));
        assert!(began.elapsed() >= Duration::from_millis(250));
    }

    #[test]
    fn zombies_do_not_run() {
        let ps = "  10 S\n  11 Z+\n  12 R\n";
        assert_eq!(live_pids(ps, &[10, 11]), vec![10]);
    }
}

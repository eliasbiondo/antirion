//! Resident memory, CPU time and open descriptors of processes: `/proc` on Linux (and the cgroup's
//! CPU accounting when the process has its own), `ps` and `lsof` elsewhere.

use std::path::Path;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::hosts::{self, Pid};

/// One reading of a process.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Reading {
    pub(crate) rss_bytes: u64,
    pub(crate) cpu_s: f64,
}

/// Linux clock ticks per second for `/proc/<pid>/stat` (USER_HZ is 100 on every mainstream build).
const USER_HZ: f64 = 100.0;

fn proc_reading(pid: u32) -> Option<Reading> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss_kib: u64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after.split_whitespace().collect();
    let utime: f64 = fields.get(11)?.parse().ok()?;
    let stime: f64 = fields.get(12)?.parse().ok()?;
    Some(Reading {
        rss_bytes: rss_kib * 1024,
        cpu_s: (utime + stime) / USER_HZ,
    })
}

/// `[[dd-]hh:]mm:ss[.ff]` → seconds.
fn parse_cputime(s: &str) -> Option<f64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<f64>().ok()?, r),
        None => (0.0, s),
    };
    let mut secs = 0.0;
    for part in rest.split(':') {
        secs = secs * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(days * 86_400.0 + secs)
}

async fn ps_reading(pid: u32) -> Option<Reading> {
    let out = tokio::process::Command::new("ps")
        .args(["-o", "rss=,time=", "-p", &pid.to_string()])
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.split_whitespace();
    let rss_kib: u64 = parts.next()?.parse().ok()?;
    let cpu_s = parse_cputime(parts.next()?)?;
    Some(Reading {
        rss_bytes: rss_kib * 1024,
        cpu_s,
    })
}

/// A reading of `pid`, or `None` when the process is gone.
pub(crate) async fn read(pid: u32) -> Option<Reading> {
    if Path::new("/proc/self/status").exists() {
        proc_reading(pid)
    } else {
        ps_reading(pid).await
    }
}

/// The sum over `pids`; `None` when any is unreadable.
pub(crate) async fn read_all(pids: &[u32]) -> Option<Reading> {
    let mut total = Reading::default();
    for &pid in pids {
        let r = read(pid).await?;
        total.rss_bytes += r.rss_bytes;
        total.cpu_s += r.cpu_s;
    }
    Some(total)
}

/// CPU time of the process's own cgroup (v2 `cpu.stat` `usage_usec`), when it is not the root's.
pub(crate) fn cgroup_cpu_s(pid: u32) -> Option<f64> {
    let cg = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let rel = cg.lines().find_map(|l| l.strip_prefix("0::"))?.trim();
    if rel == "/" || rel.is_empty() {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/sys/fs/cgroup{rel}/cpu.stat")).ok()?;
    let usec: f64 = stat
        .lines()
        .find_map(|l| l.strip_prefix("usage_usec "))?
        .trim()
        .parse()
        .ok()?;
    Some(usec / 1e6)
}

/// Open file descriptors of `pid`.
pub(crate) async fn fds(pid: u32) -> Option<u64> {
    let dir = format!("/proc/{pid}/fd");
    if Path::new("/proc/self/fd").exists() {
        return std::fs::read_dir(dir).ok().map(|d| d.count() as u64);
    }
    let out = tokio::process::Command::new("lsof")
        .args(["-n", "-P", "-p", &pid.to_string()])
        .output()
        .await
        .ok()?;
    let lines = String::from_utf8_lossy(&out.stdout).lines().count() as u64;
    lines.checked_sub(1)
}

/// Samples the summed RSS of `pids` once a second until `stop`; `(seconds since start, bytes)`.
pub(crate) async fn rss_series(
    pids: Vec<Pid>,
    start: tokio::time::Instant,
    stop: CancellationToken,
) -> Vec<(f64, u64)> {
    if pids.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut tick = tokio::time::interval_at(start, Duration::from_secs(1));
    loop {
        tokio::select! {
            () = stop.cancelled() => return out,
            t = tick.tick() => {
                if let Some(r) = hosts::read_all(&pids).await {
                    out.push((t.duration_since(start).as_secs_f64(), r.rss_bytes));
                }
            }
        }
    }
}

/// Logical CPUs of this machine.
pub(crate) fn cpus() -> f64 {
    std::thread::available_parallelism().map_or(1.0, |n| {
        #[allow(clippy::cast_precision_loss)]
        let v = n.get() as f64;
        v
    })
}

/// Whether `pid` runs in a cgroup of its own (a container), not the harness's.
pub(crate) fn own_cgroup(pid: u32) -> bool {
    let of = |p: &str| std::fs::read_to_string(format!("/proc/{p}/cgroup")).ok();
    match (of(&pid.to_string()), of("self")) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cputime_formats() {
        assert_eq!(parse_cputime("0:01.25"), Some(1.25));
        assert_eq!(parse_cputime("01:02:03"), Some(3723.0));
        assert_eq!(parse_cputime("1-00:00:01"), Some(86_401.0));
        assert_eq!(parse_cputime("x"), None);
    }

    #[tokio::test]
    async fn reads_this_process() {
        let r = read(std::process::id()).await.unwrap();
        assert!(r.rss_bytes > 1024 * 1024);
        assert!(fds(std::process::id()).await.unwrap() >= 3);
    }
}

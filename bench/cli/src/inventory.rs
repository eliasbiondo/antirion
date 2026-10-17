//! The machine inventory of a run: CPU model, kernel, instance type, system settings, limits,
//! governor, network speed, and the measured round trip to the mock; of this machine, or of another
//! one through its agent.

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

async fn sh(cmd: &str) -> Option<String> {
    let out = tokio::process::Command::new("sh")
        .args(["-c", cmd])
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

/// Round trips of a TCP connect to `addr`: p50 and p99 in microseconds.
async fn rtt(addr: SocketAddr) -> Value {
    let mut us: Vec<u128> = Vec::new();
    for _ in 0..200 {
        let t = Instant::now();
        if tokio::time::timeout(Duration::from_secs(1), tokio::net::TcpStream::connect(addr))
            .await
            .is_ok_and(|r| r.is_ok())
        {
            us.push(t.elapsed().as_micros());
        }
    }
    us.sort_unstable();
    let at = |q: f64| {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let i = ((us.len().saturating_sub(1)) as f64 * q) as usize;
        us.get(i).copied()
    };
    json!({"target": addr.to_string(), "samples": us.len(), "connect_p50_us": at(0.5), "connect_p99_us": at(0.99)})
}

/// This machine's inventory document, with the raw `sysctl -a` and `ulimit -a`.
pub(crate) async fn collect(
    machine: &str,
    mock: Option<SocketAddr>,
) -> anyhow::Result<(Value, String, String)> {
    let linux = cfg!(target_os = "linux");
    let cpuinfo = if linux { read("/proc/cpuinfo") } else { None };
    let cpu = if linux {
        cpuinfo.as_deref().and_then(|c| {
            c.lines().find_map(|l| {
                l.strip_prefix("model name")
                    .map(|r| r.trim_start_matches([' ', '\t', ':']).to_owned())
            })
        })
    } else {
        sh("sysctl -n machdep.cpu.brand_string").await
    };
    let flags = cpuinfo.as_deref().and_then(|c| {
        c.lines().find_map(|l| {
            l.strip_prefix("flags")
                .map(|r| r.trim_start_matches([' ', '\t', ':']).to_owned())
        })
    });
    let sysctl = sh("sysctl -a 2>/dev/null").await.unwrap_or_default();
    let ulimit = sh("ulimit -a").await.unwrap_or_default();
    let pick = |key: &str| {
        sysctl
            .lines()
            .find(|l| l.starts_with(key))
            .map(|l| l.split_once(['=', ':']).map_or(l, |(_, v)| v).trim().to_owned())
    };
    let nic = if linux {
        sh("for n in /sys/class/net/*; do [ -r $n/speed ] && echo \"$(basename $n) $(cat $n/speed 2>/dev/null)\"; done").await
    } else {
        None
    };
    let mut doc = json!({
        "machine": machine,
        "cpu_model": cpu,
        "cpu_flags": flags,
        "logical_cpus": std::thread::available_parallelism().map(std::num::NonZero::get).ok(),
        "nproc": sh("nproc").await,
        "steal_and_load": sh("head -1 /proc/stat; cat /proc/loadavg").await,
        "memory": sh("head -3 /proc/meminfo").await,
        "kernel": sh("uname -srvm").await,
        "instance_type": read("/sys/devices/virtual/dmi/id/product_name").or(sh("sysctl -n hw.model").await),
        "governor": read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        "nic_speed_mbps": nic,
        "settings": {
            "somaxconn": pick("net.core.somaxconn").or_else(|| pick("kern.ipc.somaxconn")),
            "ip_local_port_range": pick("net.ipv4.ip_local_port_range").or_else(|| pick("net.inet.ip.portrange.first")),
            "tcp_tw_reuse": pick("net.ipv4.tcp_tw_reuse"),
            "open_files": sh("ulimit -n").await,
        },
        "rtt_to_mock": match mock {
            Some(m) => rtt(m).await,
            None => Value::Null,
        },
    });
    if let Some(raw) = cpuinfo {
        doc["cpuinfo"] = Value::String(raw);
    }
    Ok((doc, sysctl, ulimit))
}

/// Writes `inventory/<machine>.json` (and the raw `sysctl -a` and `ulimit -a`).
pub(crate) fn write_doc(
    dir: &Path,
    machine: &str,
    doc: &Value,
    sysctl: &str,
    ulimit: &str,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(format!("{machine}-sysctl.txt")), sysctl)?;
    std::fs::write(dir.join(format!("{machine}-ulimit.txt")), ulimit)?;
    std::fs::write(
        dir.join(format!("{machine}.json")),
        serde_json::to_string_pretty(doc)? + "\n",
    )?;
    Ok(())
}

/// This machine's inventory, written as `machine`.
pub(crate) async fn write(dir: &Path, machine: &str, mock: SocketAddr) -> anyhow::Result<()> {
    let (doc, sysctl, ulimit) = collect(machine, Some(mock)).await?;
    write_doc(dir, machine, &doc, &sysctl, &ulimit)
}

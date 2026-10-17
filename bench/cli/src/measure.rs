//! One measured run: the load generator against one side (the gateway nodes or the mock), with the
//! samplers armed on its measurement window.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use antirion_bench_loadgen::{
    ClosedLoop, Nodes, OpenLoop, Protocol, RunReport, Source, closed_loop, open_loop,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::ctx::Ctx;
use crate::gateway::READY;
use crate::hosts::{self, Pid};
use crate::procs;
use crate::sampler;

/// The load shape of a run.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Load {
    Open(OpenLoop),
    Closed(ClosedLoop),
}

impl Load {
    fn windows(&self) -> (Duration, Duration) {
        match self {
            Load::Open(o) => (o.warmup, o.measure),
            Load::Closed(c) => (c.warmup, c.measure),
        }
    }
}

/// One side of a cell.
pub(crate) struct Side {
    pub(crate) nodes: Vec<SocketAddr>,
    /// Processes whose memory and CPU are the side's (the gateway nodes); none for the mock side.
    pub(crate) pids: Vec<Pid>,
    pub(crate) source: Arc<dyn Source>,
    /// Probe these nodes' readiness once a second during the window.
    pub(crate) probe: Vec<SocketAddr>,
    /// Admin listeners, for runtime counters.
    pub(crate) admin: Vec<SocketAddr>,
    /// Count descriptors and tasks at the window's edges.
    pub(crate) counters: bool,
}

/// What a run measured besides the generator's own report.
#[derive(Debug, Clone)]
pub(crate) struct Measured {
    pub(crate) report: RunReport,
    /// `(seconds since the window start, bytes)`, from the start of the warm-up.
    pub(crate) rss: Vec<(f64, u64)>,
    pub(crate) gateway_cpu_s: Option<f64>,
    pub(crate) cpu_source: String,
    pub(crate) mock_cpu_pct: Option<f64>,
    pub(crate) generator_cpu_pct: f64,
    pub(crate) upstream_requests: Option<f64>,
    pub(crate) readiness_failures: Option<f64>,
    /// Open descriptors and live runtime tasks of the side's processes at the window's start and end.
    pub(crate) fds: (Option<f64>, Option<f64>),
    pub(crate) tasks: (Option<f64>, Option<f64>),
}

#[derive(Debug, Clone, Copy, Default)]
struct Snapshot {
    gateway_cpu: Option<f64>,
    cgroup: bool,
    mock_cpu: Option<f64>,
    upstream: Option<f64>,
    fds: Option<f64>,
    tasks: Option<f64>,
}

/// Live runtime tasks from the gateway's metrics, when it exports them.
pub(crate) async fn tasks(ctx: &Ctx, admin: &[SocketAddr]) -> Option<f64> {
    let token = ctx.admin_token_for().ok()?;
    let mut total = None;
    for a in admin {
        let (status, body) = procs::get(*a, "/metrics", Some(&token)).await.ok()?;
        if status != 200 {
            return None;
        }
        let text = String::from_utf8_lossy(&body);
        let v: f64 = text
            .lines()
            .find(|l| l.starts_with("antirion_runtime_tasks"))?
            .split_whitespace()
            .last()?
            .parse()
            .ok()?;
        total = Some(total.unwrap_or(0.0) + v);
    }
    total
}

/// Open descriptors summed over `pids`.
pub(crate) async fn fds(pids: &[Pid]) -> Option<f64> {
    if pids.is_empty() {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    hosts::fds(pids).await.map(|n| n as f64)
}

async fn snapshot(ctx: &Ctx, pids: &[Pid], admin: &[SocketAddr], counters: bool) -> Snapshot {
    let usage = if pids.is_empty() {
        None
    } else {
        hosts::usage(pids).await
    };
    let cgroup = usage.and_then(|(_, c)| c);
    let gateway_cpu = cgroup.or(usage.map(|(r, _)| r.cpu_s));
    let mock_cpu = match ctx.mock_pid {
        Some(pid) => hosts::read_all(&[pid]).await.map(|r| r.cpu_s),
        None => None,
    };
    let upstream = procs::get(ctx.mock_addr, "/__bench/stats", None)
        .await
        .ok()
        .and_then(|(_, body)| serde_json::from_slice::<Value>(&body).ok())
        .and_then(|v| v.get("upstream_requests")?.as_f64());
    let (fds, tasks) = if counters {
        (fds(pids).await, tasks(ctx, admin).await)
    } else {
        (None, None)
    };
    Snapshot {
        gateway_cpu,
        cgroup: cgroup.is_some(),
        mock_cpu,
        upstream,
        fds,
        tasks,
    }
}

async fn probe(nodes: Vec<SocketAddr>, from: Instant, to: Instant) -> Option<f64> {
    if nodes.is_empty() {
        return None;
    }
    tokio::time::sleep_until(tokio::time::Instant::from_std(from)).await;
    let mut failures = 0.0;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    while Instant::now() < to {
        tick.tick().await;
        for n in &nodes {
            if !matches!(procs::get(*n, READY, None).await, Ok((200, _))) {
                failures += 1.0;
            }
        }
    }
    Some(failures)
}

/// Runs `load` against `side`, sampling the side's processes and the mock over the window.
pub(crate) async fn run(ctx: &Ctx, side: Side, load: Load, protocol: Protocol) -> anyhow::Result<Measured> {
    let nodes = Arc::new(Nodes::connect(&side.nodes, protocol).await?);
    let (warmup, measure) = load.windows();
    let start = Instant::now() + Duration::from_millis(300);
    let (wstart, wend) = (start + warmup, start + warmup + measure);
    let stop = CancellationToken::new();
    let rss = sampler::rss_series(
        side.pids.clone(),
        tokio::time::Instant::from_std(start),
        stop.clone(),
    );
    let at = |t: Instant| {
        let pids = side.pids.clone();
        let admin = side.admin.clone();
        let counters = side.counters;
        async move {
            tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await;
            snapshot(ctx, &pids, &admin, counters).await
        }
    };
    let driven = async {
        let r = match load {
            Load::Open(cfg) => open_loop(cfg, Arc::clone(&side.source), Arc::clone(&nodes), start).await,
            Load::Closed(cfg) => closed_loop(cfg, Arc::clone(&side.source), Arc::clone(&nodes), start).await,
        };
        stop.cancel();
        r
    };
    let (report, series, before, after, failures) = tokio::join!(
        driven,
        rss,
        at(wstart),
        at(wend),
        probe(side.probe.clone(), wstart, wend)
    );
    if let Ok(n) = Arc::try_unwrap(nodes) {
        n.close().await;
    }
    let secs = report
        .window
        .end
        .saturating_duration_since(report.window.start)
        .as_secs_f64();
    let capacity = secs * sampler::cpus();
    let pct = |s: f64| if capacity > 0.0 { s / capacity * 100.0 } else { 0.0 };
    let diff = |a: Option<f64>, b: Option<f64>| Some(b? - a?);
    let window_offset = warmup.as_secs_f64();
    Ok(Measured {
        generator_cpu_pct: pct(report.generator_cpu_s),
        gateway_cpu_s: diff(before.gateway_cpu, after.gateway_cpu),
        cpu_source: if before.cgroup {
            "cgroup".into()
        } else if cfg!(target_os = "linux") {
            "proc".into()
        } else {
            "ps".into()
        },
        mock_cpu_pct: diff(before.mock_cpu, after.mock_cpu).map(pct),
        upstream_requests: diff(before.upstream, after.upstream),
        readiness_failures: failures,
        fds: (before.fds, after.fds),
        tasks: (before.tasks, after.tasks),
        rss: series.into_iter().map(|(t, b)| (t - window_offset, b)).collect(),
        report,
    })
}

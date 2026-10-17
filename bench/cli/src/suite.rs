//! A run from start to end: the result directory and its manifest, the inventory, the mock, the
//! self-tests, the cells (alternating pairs together), and the report.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use antirion_bench_corpus::SEED;
use antirion_bench_report::model::{CellDef, Kind, RunManifest, Validity, write_json};
use anyhow::{Context, bail};
use serde_json::json;

use crate::cells;
use crate::config::{self, Launch, Paths, Topology};
use crate::ctx::{Ctx, Target, Windows};
use crate::hosts::{self, Host, Running};
use crate::procs::{self, Spawn};
use crate::{inventory, selftest};

/// What a run is asked to do.
#[derive(Debug, Clone)]
pub(crate) struct RunSpec {
    pub(crate) suite: String,
    pub(crate) topology: String,
    pub(crate) target: Target,
    pub(crate) gateway_bin: Option<PathBuf>,
    pub(crate) out: Option<PathBuf>,
    pub(crate) work: Option<PathBuf>,
    pub(crate) windows: Windows,
    pub(crate) command: Vec<String>,
}

/// The admin token of locally started nodes (they listen on loopback or a private network only).
const ADMIN_TOKEN: &str = "bench-admin-token-0123456789abcdef0123456789";

fn now_utc() -> String {
    jiff::Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

async fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The checkout's commit (`-dirty` when it has changes), or what a `COMMIT` file in a copy says.
async fn commit(dir: &Path) -> String {
    let Some(head) = git(dir, &["rev-parse", "HEAD"]).await else {
        return std::fs::read_to_string(dir.join("COMMIT"))
            .ok()
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "unknown".to_owned());
    };
    let dirty = git(dir, &["status", "--porcelain"])
        .await
        .is_some_and(|s| !s.is_empty());
    if dirty { format!("{head}-dirty") } else { head }
}

/// Raises this process's descriptor limit (children inherit it), and on Linux the stack limit that
/// bounds a child's environment, which carries the keys of the large profiles.
pub(crate) fn raise_limits() {
    let _ = rlimit::increase_nofile_limit(1_048_576);
    #[cfg(target_os = "linux")]
    if let Ok((soft, hard)) = rlimit::Resource::STACK.get() {
        let want = (256u64 << 20).min(hard);
        if soft < want {
            let _ = rlimit::Resource::STACK.set(want, hard);
        }
    }
}

/// Starts the mock here or, with an agent, on its machine (from the same path as here).
async fn start_mock(topo: &Topology, work: &Path) -> anyhow::Result<(Option<Running>, SocketAddr)> {
    let addr = topo.mock.listen;
    if topo.mock.launch == Launch::External {
        procs::wait_ok(addr, "/__bench/health", Duration::from_secs(10), None).await?;
        return Ok((None, addr));
    }
    let host = Host::of(topo.mock.agent);
    let exe = std::env::current_exe()?;
    let program = exe
        .parent()
        .map(|d| d.join("antirion-bench-mock"))
        .filter(|p| host != Host::Local || p.is_file())
        .context("antirion-bench-mock is not next to antirion-bench; build the bench workspace")?;
    let mut args = vec!["--listen".to_owned(), addr.to_string()];
    if let Some(w) = topo.mock.workers {
        args.extend(["--workers".to_owned(), w.to_string()]);
    }
    let dir = hosts::work_dir(host, work).await?;
    let mut p = hosts::spawn(
        host,
        &Spawn {
            name: "mock",
            program,
            args,
            nice: topo.mock.nice,
            cpus: topo.mock.cpus.as_deref(),
            env: None,
            add_env: &[],
            cwd: None,
            log: dir.join("mock.log"),
        },
        &[],
        &[],
    )
    .await?;
    if let Err(e) = procs::wait_ok(addr, "/__bench/health", Duration::from_secs(20), Some(&mut p)).await {
        p.stop(Duration::from_secs(2)).await;
        return Err(e);
    }
    Ok((Some(p), addr))
}

/// Inventories of the other machines the topology runs processes on, through their agents.
async fn remote_inventories(ctx: &Ctx) {
    let mut agents: Vec<(String, SocketAddr)> = Vec::new();
    if let Some(a) = ctx.topo.mock.agent {
        agents.push(("mock".to_owned(), a));
    }
    for (i, n) in ctx.topo.gateway.nodes.iter().enumerate() {
        if let Some(a) = n.agent
            && !agents.iter().any(|(_, b)| *b == a)
        {
            agents.push((format!("gateway-{}", i + 1), a));
        }
    }
    for (machine, agent) in agents {
        match hosts::inventory(agent, &machine, ctx.mock_addr).await {
            Ok(inv) => {
                let _ = inventory::write_doc(
                    &ctx.out.join("inventory"),
                    &machine,
                    &inv.doc,
                    &inv.sysctl,
                    &inv.ulimit,
                );
            }
            Err(e) => eprintln!("inventory of {machine} ({agent}): {e:#}"),
        }
    }
}

/// Builds the context of a run: directories, manifest, inventory and the mock.
pub(crate) async fn open(paths: Paths, spec: &RunSpec) -> anyhow::Result<Ctx> {
    open_with(paths, spec, None).await
}

/// Like [`open`], with the mock of another run (`shared`) instead of one of its own.
pub(crate) async fn open_with(paths: Paths, spec: &RunSpec, shared: Option<&Ctx>) -> anyhow::Result<Ctx> {
    raise_limits();
    let topo = Topology::load(&paths, &spec.topology)?;
    let stamp = jiff::Timestamp::now().strftime("%Y%m%dT%H%M%SZ").to_string();
    let out = spec.out.clone().unwrap_or_else(|| {
        paths.bench.join("results").join(format!(
            "{stamp}-{}-{}-{}",
            spec.suite,
            topo.name,
            spec.target.name()
        ))
    });
    std::fs::create_dir_all(out.join("cells"))?;
    let work = spec
        .work
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("antirion-bench-{}", std::process::id())));
    std::fs::create_dir_all(&work)?;
    let gateway_bin = spec.gateway_bin.clone().unwrap_or_else(|| {
        paths
            .repo
            .join(topo.gateway.bin.as_deref().unwrap_or("target/release/antirion"))
    });
    let (mock, mock_addr, mock_pid) = match shared {
        Some(other) => (None, other.mock_addr, other.mock_pid),
        None => {
            let (m, addr) = start_mock(&topo, &work).await?;
            let pid = m.as_ref().map(Running::pid);
            (m, addr, pid)
        }
    };
    let ctx = Ctx {
        scenarios: config::scenarios(&paths)?,
        mock_pid,
        mock: Mutex::new(mock),
        paths,
        topo,
        out,
        work,
        gateway_bin,
        mock_addr,
        target: spec.target.clone(),
        windows: spec.windows.clone(),
        suite: spec.suite.clone(),
        admin_token: ADMIN_TOKEN.to_owned(),
        commands: Mutex::new(vec![spec.command.clone()]),
        debug_build: cfg!(debug_assertions),
        epoch: (
            Instant::now(),
            std::time::SystemTime::from(jiff::Timestamp::now()),
        ),
    };
    if let Ok(m) = ctx.mock.lock()
        && let Some(p) = m.as_ref()
    {
        ctx.record_command(p.argv());
    }
    write_manifest(&ctx, spec, None).await?;
    inventory::write(&ctx.out.join("inventory"), "generator", ctx.mock_addr).await?;
    if shared.is_none() {
        remote_inventories(&ctx).await;
    }
    Ok(ctx)
}

async fn write_manifest(ctx: &Ctx, spec: &RunSpec, ended: Option<String>) -> anyhow::Result<()> {
    let path = ctx.out.join("run.json");
    let started = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<RunManifest>(&t).ok())
        .map_or_else(now_utc, |m| m.started_at);
    let version = match ctx.target {
        Target::Antirion => tokio::process::Command::new(&ctx.gateway_bin)
            .arg("--version")
            .output()
            .await
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .last()
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "unknown".to_owned()),
        Target::External(ref e) => e
            .version
            .clone()
            .unwrap_or_else(|| "see the driver's VERSION".to_owned()),
    };
    let manifest = RunManifest {
        schema_version: 1,
        suite: spec.suite.clone(),
        topology: ctx.topo.name.clone(),
        target: ctx.target.name().to_owned(),
        target_version: version,
        target_commit: match ctx.target {
            Target::Antirion => commit(&ctx.paths.repo).await,
            Target::External(_) => "n/a".to_owned(),
        },
        harness_commit: commit(&ctx.paths.bench).await,
        corpus_seed: SEED,
        started_at: started,
        ended_at: ended.unwrap_or_default(),
        command: spec.command.clone(),
        reference: ctx.topo.reference && !ctx.debug_build,
        not_publishable: ctx.not_publishable(),
    };
    write_json(&path, &manifest)?;
    Ok(())
}

/// Stops the mock, writes the commands and the final manifest, renders the report.
pub(crate) async fn close(ctx: &Ctx, spec: &RunSpec) -> anyhow::Result<antirion_bench_report::Outcome> {
    let mock = ctx.mock.lock().ok().and_then(|mut m| m.take());
    if let Some(p) = mock {
        p.stop(Duration::from_secs(5)).await;
    }
    let configs = ctx.out.join("configs");
    std::fs::create_dir_all(&configs)?;
    let commands = ctx.commands.lock().map(|c| c.clone()).unwrap_or_default();
    write_json(&configs.join("commands.json"), &commands)?;
    write_json(
        &configs.join("generator.json"),
        &json!({"topology": ctx.topo.file.display().to_string(), "generator": {
            "workers": ctx.topo.generator.workers, "max_connections": ctx.topo.generator.max_connections,
            "h2c_connections": ctx.topo.generator.h2c_connections, "h2c_streams": ctx.topo.generator.h2c_streams},
            "warmup_s": ctx.windows.warmup.map(|d| d.as_secs_f64()),
            "measure_s": ctx.windows.measure.map(|d| d.as_secs_f64()),
            "repetitions": ctx.windows.reps}),
    )?;
    let _ = std::fs::copy(&ctx.topo.file, configs.join("topology.yaml"));
    write_manifest(ctx, spec, Some(now_utc())).await?;
    let _ = std::fs::remove_dir_all(&ctx.work);
    Ok(antirion_bench_report::render(&ctx.out, None, &[])?)
}

/// Runs a pair of cells whose repetitions alternate, each repetition on a fresh gateway.
async fn alternate(ctx: &Ctx, a: &CellDef, b: &CellDef) -> anyhow::Result<()> {
    let mut dirs = Vec::new();
    for def in [a, b] {
        let dir = ctx.out.join("cells").join(&def.cell);
        std::fs::create_dir_all(&dir)?;
        write_json(&dir.join("cell.json"), &cells::resolved(ctx, def))?;
        dirs.push(dir);
    }
    let skipped = [a, b]
        .into_iter()
        .find_map(|def| cells::skip_reason(ctx, def).map(|why| (def, why)));
    if let Some((cause, why)) = skipped {
        // A pair is measured together or not at all, and both cells say why.
        for (def, dir) in [a, b].into_iter().zip(&dirs) {
            let reason = if def.cell == cause.cell {
                why.clone()
            } else {
                format!("alternates with {}, which is skipped: {why}", cause.cell)
            };
            write_json(&dir.join("skipped.json"), &json!({"reason": reason}))?;
            eprintln!("{}: skipped ({reason})", def.cell);
        }
        return Ok(());
    }
    eprintln!("{} and {}: alternating", a.cell, b.cell);
    for rep in 1..=ctx.reps(a) {
        for (def, dir) in [a, b].into_iter().zip(&dirs) {
            if let Err(e) = cells::traffic(ctx, def, dir, rep..=rep).await {
                for d in &dirs {
                    cells::fail(d, &format!("{e:#}"))?;
                }
                eprintln!("{}: failed: {e:#}", def.cell);
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Runs every cell for every target of `ctxs`, interleaved: per cell and repetition, each target in
/// turn on a fresh gateway, so that drift of the machine reaches every target alike.
pub(crate) async fn interleave(ctxs: &[Ctx], list: &[CellDef]) -> anyhow::Result<()> {
    for def in list {
        let mut live = Vec::new();
        for ctx in ctxs {
            let dir = ctx.out.join("cells").join(&def.cell);
            std::fs::create_dir_all(&dir)?;
            write_json(&dir.join("cell.json"), &cells::resolved(ctx, def))?;
            if let Some(why) = cells::skip_reason(ctx, def) {
                write_json(&dir.join("skipped.json"), &json!({"reason": why}))?;
                eprintln!("{} on {}: skipped ({why})", def.cell, ctx.target.name());
            } else if def.kind == Kind::Micro {
                let why = "micro-benchmarks are not interleaved".to_owned();
                write_json(&dir.join("skipped.json"), &json!({"reason": why}))?;
                eprintln!("{} on {}: skipped ({why})", def.cell, ctx.target.name());
            } else {
                live.push((ctx, dir));
            }
        }
        let reps = live.iter().map(|(c, _)| c.reps(def)).max().unwrap_or(0);
        for rep in 1..=reps {
            for (ctx, dir) in &live {
                eprintln!("{} rep {rep}: {}", def.cell, ctx.target.name());
                let r = match def.kind {
                    Kind::Saturation => cells::saturation(ctx, def, dir, rep..=rep).await,
                    Kind::Idle | Kind::Startup | Kind::Apply => {
                        crate::kinds::run(ctx, def, dir, rep..=rep).await
                    }
                    _ => cells::traffic(ctx, def, dir, rep..=rep).await,
                };
                if let Err(e) = r {
                    let why = format!("{e:#}");
                    eprintln!("{} rep {rep} on {}: failed: {why}", def.cell, ctx.target.name());
                    let rep_dir = dir.join(format!("rep-{rep}"));
                    std::fs::create_dir_all(&rep_dir)?;
                    write_json(
                        &rep_dir.join("validity.json"),
                        &Validity {
                            valid: false,
                            reasons: vec![why],
                        },
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Runs `cells` in order.
pub(crate) async fn run_cells(ctx: &Ctx, list: &[CellDef]) -> anyhow::Result<()> {
    let mut done = BTreeSet::new();
    for c in list {
        if done.contains(&c.cell) {
            continue;
        }
        let partner = c
            .alternate_with
            .as_ref()
            .and_then(|p| list.iter().find(|o| &o.cell == p));
        match partner {
            Some(p) if !done.contains(&p.cell) => {
                alternate(ctx, c, p).await?;
                done.insert(p.cell.clone());
            }
            _ => cells::run(ctx, c).await?,
        }
        done.insert(c.cell.clone());
    }
    Ok(())
}

/// The self-tests; a failure aborts the run.
pub(crate) async fn selftest_or_abort(ctx: &Ctx, with_gateway: bool) -> anyhow::Result<()> {
    if !selftest::run(ctx, with_gateway).await? {
        bail!(
            "the self-tests failed (see {}); the run is aborted",
            ctx.out.join("selftest.json").display()
        );
    }
    Ok(())
}

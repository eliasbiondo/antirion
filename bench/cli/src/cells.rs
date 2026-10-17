//! Running a cell: writing its definition, deciding whether it applies here, then the traffic kinds
//! (open loop, closed loop, held streams, the saturation ladder) with their alternating direct runs.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use antirion_bench_loadgen::{ClosedLoop, OpenLoop, Protocol};
use antirion_bench_mock::Profile;
use antirion_bench_report::model::{CellDef, Kind, Validity, write_json};
use anyhow::{Context, bail};
use http::HeaderName;
use serde_json::json;

use crate::ctx::{Ctx, Target};
use crate::gateway::{self, Gateway};
use crate::hosts::{self, Host, Pid, Running};
use crate::measure::{self, Load, Measured, Side};
use crate::output;
use crate::procs::{self, Spawn};
use crate::targets::{self, External, StartCmd};

use crate::source::{self, KeyChoice, Loaded, ScenarioSource, SourceCfg};

/// The definition as written to `cell.json`: run facts filled in.
pub(crate) fn resolved(ctx: &Ctx, def: &CellDef) -> CellDef {
    let mut d = def.clone();
    d.topology.clone_from(&ctx.topo.name);
    d.reference = ctx.topo.reference && !ctx.debug_build;
    d.repetitions = ctx.reps(def);
    d
}

/// Why `def` does not run here, if it does not.
pub(crate) fn skip_reason(ctx: &Ctx, def: &CellDef) -> Option<String> {
    if let Some(t) = &def.needs_topology
        && t != &ctx.topo.name
    {
        return Some(format!("needs the {t} topology"));
    }
    if def.profile.as_deref() == Some("P-cluster") && ctx.topo.store.is_none() {
        return Some("P-cluster needs PostgreSQL and Valkey; the topology gives none (`store`)".to_owned());
    }
    if let Target::External(_) = ctx.target {
        if matches!(def.kind, Kind::Startup | Kind::Apply | Kind::Micro) {
            return Some("not applicable to an external target".to_owned());
        }
        if !def.compare {
            return Some("not a comparison cell".to_owned());
        }
        if let Some(s) = &def.scenario
            && (s.starts_with("S7") || s == "S8-403" || s == "S8-429")
        {
            return Some(format!(
                "{s} needs a feature the external target's driver does not configure"
            ));
        }
    }
    if cfg!(target_os = "macos")
        && let Some(p) = &def.profile
        && let Ok(r) = ctx.render(def, p)
    {
        let bytes: usize = r.env.iter().map(|(k, v)| k.len() + v.len() + 10).sum();
        if bytes > 960 * 1024 {
            return Some(format!(
                "{p}'s {} keys do not fit in this system's process environment limit",
                r.env.len()
            ));
        }
    }
    None
}

/// Runs `def` completely: definition, repetitions, files.
pub(crate) async fn run(ctx: &Ctx, def: &CellDef) -> anyhow::Result<()> {
    let dir = ctx.out.join("cells").join(&def.cell);
    std::fs::create_dir_all(&dir)?;
    write_json(&dir.join("cell.json"), &resolved(ctx, def))?;
    if let Some(why) = skip_reason(ctx, def) {
        write_json(&dir.join("skipped.json"), &json!({"reason": why}))?;
        eprintln!("{}: skipped ({why})", def.cell);
        return Ok(());
    }
    eprintln!("{}: running", def.cell);
    let reps = 1..=ctx.reps(def);
    let result = match def.kind {
        Kind::Open | Kind::Closed | Kind::Hold => traffic(ctx, def, &dir, reps).await,
        Kind::Saturation => saturation(ctx, def, &dir, reps).await,
        Kind::Idle | Kind::Startup | Kind::Apply => crate::kinds::run(ctx, def, &dir, reps).await,
        Kind::Micro => crate::micro::run(ctx, def, &dir).await,
    };
    if let Err(e) = result {
        fail(&dir, &format!("{e:#}"))?;
        eprintln!("{}: failed: {e:#}", def.cell);
    }
    Ok(())
}

/// Records a cell that could not complete as invalid, with the reason.
pub(crate) fn fail(dir: &Path, why: &str) -> anyhow::Result<()> {
    let rep = dir.join("rep-1");
    std::fs::create_dir_all(&rep)?;
    if !rep.join("validity.json").exists() {
        write_json(
            &rep.join("validity.json"),
            &Validity {
                valid: false,
                reasons: vec![why.to_owned()],
            },
        )?;
    }
    std::fs::write(dir.join("error.txt"), why)?;
    Ok(())
}

/// The gateway side of a cell: a started gateway, or the external target (started, when it has a
/// command).
pub(crate) struct Under {
    pub(crate) gw: Option<Gateway>,
    pub(crate) started: Option<Running>,
    pub(crate) nodes: Vec<std::net::SocketAddr>,
    pub(crate) admin: Vec<std::net::SocketAddr>,
    pub(crate) pids: Vec<Pid>,
    pub(crate) keys: Vec<String>,
    pub(crate) sessions: Vec<String>,
    pub(crate) models: Vec<String>,
}

pub(crate) async fn bring_up(
    ctx: &Ctx,
    def: &CellDef,
    scen: Option<&Loaded>,
    tag: &str,
) -> anyhow::Result<Under> {
    match &ctx.target {
        Target::External(e) => {
            let started = match &e.start {
                Some(cmd) => Some(start_external(ctx, e, cmd, tag).await?),
                None => None,
            };
            Ok(Under {
                gw: None,
                pids: targets::resolve_pids(e, started.as_ref().map(Running::pid)).await,
                started,
                nodes: e.nodes.clone(),
                admin: Vec::new(),
                keys: match scen.map(|s| &s.key) {
                    Some(KeyChoice::Unknown) => {
                        vec![antirion_bench_corpus::keys::unknown(antirion_bench_corpus::SEED)]
                    }
                    _ => vec![e.key.clone()],
                },
                sessions: Vec::new(),
                models: Vec::new(),
            })
        }
        Target::Antirion => {
            let profile = def.profile.as_deref().context("the cell names no profile")?;
            let gw = ctx.start_gateway(def, profile, tag).await?;
            let mut keys = match scen {
                Some(s) => source::keys_for(s, &gw.rendered.used_keys, &gw.rendered.named)?,
                None => gw.rendered.used_keys.clone(),
            };
            let mut sessions = Vec::new();
            if scen.and_then(|s| s.session.as_deref()) == Some("mcp_legacy") {
                keys.truncate(1000);
                sessions = gateway::mcp_sessions(&gw, &keys).await?;
            }
            Ok(Under {
                started: None,
                nodes: gw.data.clone(),
                admin: gw.admin.clone(),
                pids: gw.pids.clone(),
                models: gw.rendered.models.clone(),
                keys,
                sessions,
                gw: Some(gw),
            })
        }
    }
}

/// Starts an external target under the topology's priority and CPU set and waits until it serves.
async fn start_external(ctx: &Ctx, e: &External, cmd: &StartCmd, tag: &str) -> anyhow::Result<Running> {
    let (program, args) = cmd
        .command
        .split_first()
        .context("the target's command is empty")?;
    let host = Host::of(e.agent);
    let log = hosts::work_dir(host, &ctx.work)
        .await?
        .join(format!("{}-{tag}.log", e.name));
    let spawn = Spawn {
        name: &e.name,
        program: program.into(),
        args: args.to_vec(),
        nice: ctx.topo.gateway.nice,
        cpus: ctx.topo.gateway.cpus.as_deref(),
        env: None,
        add_env: &cmd.env,
        cwd: cmd.cwd.as_deref(),
        log: log.clone(),
    };
    let mut p = hosts::spawn(host, &spawn, &[], &[]).await?;
    ctx.record_command(p.argv());
    for node in &e.nodes {
        if let Err(err) = procs::wait_ok(*node, &cmd.ready, cmd.ready_timeout, Some(&mut p)).await {
            if host == Host::Local {
                let _ = std::fs::copy(
                    &log,
                    ctx.out.join("configs").join(format!("{}-{tag}.log", e.name)),
                );
            }
            p.stop(Duration::from_secs(5)).await;
            return Err(err);
        }
    }
    tokio::time::sleep(cmd.settle).await;
    Ok(p)
}

/// How long a stopped target's listeners may take to close before the next target starts.
const PORT_RELEASE: Duration = Duration::from_secs(30);

/// Stops what `bring_up` started and returns once its processes have exited and its listen
/// addresses refuse connections (or, failing that, after a bounded wait), so the next target can
/// bind them.
pub(crate) async fn bring_down(ctx: &Ctx, u: Under) {
    let started = u.gw.as_ref().is_some_and(|g| !g.pids.is_empty()) || u.started.is_some();
    if let Some(gw) = u.gw {
        ctx.stop_gateway(gw).await;
    }
    if let Some(p) = u.started {
        p.stop(Duration::from_secs(15)).await;
        hosts::kill_all(&u.pids).await;
    }
    if started {
        let addrs: Vec<_> = u.nodes.iter().chain(&u.admin).copied().collect();
        if let Err(addr) = procs::wait_free(&addrs, PORT_RELEASE).await {
            eprintln!("{addr} still accepts connections {PORT_RELEASE:?} after its target stopped");
        }
    }
}

/// Resident memory of the gateway once it settled after start.
pub(crate) async fn idle_rss(pids: &[Pid]) -> Option<f64> {
    if pids.is_empty() {
        return None;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut samples = Vec::new();
    for _ in 0..3 {
        if let Some(r) = hosts::read_all(pids).await {
            #[allow(clippy::cast_precision_loss)]
            samples.push(r.rss_bytes as f64);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = samples.len() as f64;
    (!samples.is_empty()).then(|| samples.iter().sum::<f64>() / n)
}

fn protocol(ctx: &Ctx, def: &CellDef) -> Protocol {
    if def.protocol == "h2c" {
        Protocol::H2c {
            connections: ctx.topo.generator.h2c_connections,
            streams: ctx.topo.generator.h2c_streams,
        }
    } else {
        // Every open HTTP/1.1 stream holds a connection, so a cell with more concurrent streams
        // than the generator's cap gets one connection per stream instead of queueing them.
        let cap = ctx.topo.generator.max_connections;
        let streams = def
            .level
            .concurrent_streams
            .and_then(|n| usize::try_from(n + n / 10).ok())
            .unwrap_or(0);
        Protocol::Http1 {
            max_connections: def.level.concurrency.unwrap_or(cap.max(streams)),
        }
    }
}

/// The load of a traffic cell at `rate` (open loop) or its concurrency (closed loop).
fn load(ctx: &Ctx, def: &CellDef, scripted: Duration, rate: Option<f64>) -> anyhow::Result<Load> {
    let timeout = scripted + Duration::from_secs(30);
    let (warmup, mut measure) = (ctx.warmup(def), ctx.measure(def));
    if def.kind == Kind::Closed {
        let concurrency = def
            .level
            .concurrency
            .context("a closed-loop cell needs level.concurrency")?;
        return Ok(Load::Closed(ClosedLoop {
            concurrency,
            warmup,
            measure,
            timeout,
        }));
    }
    let rate = match (rate, def.level.rps, def.level.concurrent_streams) {
        (Some(r), _, _) | (None, Some(r), _) => r,
        (None, None, Some(n)) => {
            #[allow(clippy::cast_precision_loss)]
            let n = n as f64;
            n / scripted.as_secs_f64().max(0.001)
        }
        _ => bail!("cell {} has no rate", def.cell),
    };
    if let Some(total) = def.level.total_requests
        && ctx.windows.measure.is_none()
    {
        #[allow(clippy::cast_precision_loss)]
        let secs = total as f64 / rate;
        measure = Duration::from_secs_f64(secs);
    }
    if def.kind == Kind::Hold {
        #[allow(clippy::cast_precision_loss)]
        let open_all = def.level.concurrent_streams.unwrap_or(0) as f64 / rate;
        let held = ctx
            .windows
            .measure
            .unwrap_or_else(|| Duration::from_secs_f64(open_all + 30.0));
        return Ok(Load::Open(OpenLoop {
            rate,
            warmup: Duration::ZERO,
            measure: held,
            timeout: held + Duration::from_secs(60),
            hold: true,
        }));
    }
    Ok(Load::Open(OpenLoop {
        rate,
        warmup,
        measure,
        timeout,
        hold: false,
    }))
}

struct Prepared {
    scen: Loaded,
    mock: Profile,
    gw_source: Arc<ScenarioSource>,
    direct_source: Option<Arc<ScenarioSource>>,
}

async fn prepare(ctx: &Ctx, def: &CellDef, u: &Under) -> anyhow::Result<Prepared> {
    let name = def.scenario.as_deref().context("the cell names no scenario")?;
    let mut scen = source::load(&ctx.paths, &ctx.scenarios, name)?;
    let mock: Profile = def.mock_profile.as_deref().unwrap_or("L0").parse()?;
    let absolute = def.protocol == "h2c";
    let mut gw_cfg = SourceCfg::new(u.nodes.clone(), absolute, def.disconnect_fraction);
    if let Target::External(e) = &ctx.target {
        scen.rename_models(|m| e.model_for(m, mock).map(str::to_owned));
        if let Some(&status) = e.statuses.get(name) {
            scen.expect_status(status);
        }
        gw_cfg.auth_header = HeaderName::from_bytes(e.auth_header.to_ascii_lowercase().as_bytes())
            .with_context(|| format!("{} is not a header name", e.auth_header))?;
    }
    let gw_source = Arc::new(ScenarioSource::gateway(
        &scen,
        &gw_cfg,
        &u.keys,
        &u.sessions,
        &u.models,
    )?);
    let direct_source = if scen.has_direct {
        let session = if scen.session.as_deref() == Some("mcp_legacy") {
            Some(gateway::mcp_session(ctx.mock_addr, &format!("/{}/mcp", mock.name()), None).await?)
        } else {
            None
        };
        let cfg = SourceCfg {
            nodes: vec![ctx.mock_addr],
            ..gw_cfg.clone()
        };
        Some(Arc::new(ScenarioSource::direct(
            &scen,
            &cfg,
            mock,
            session.as_deref(),
        )?))
    } else {
        None
    };
    Ok(Prepared {
        scen,
        mock,
        gw_source,
        direct_source,
    })
}

async fn gateway_run(
    ctx: &Ctx,
    def: &CellDef,
    u: &Under,
    p: &Prepared,
    load: Load,
) -> anyhow::Result<Measured> {
    let probe = def.gates.iter().any(|g| g.metric.contains("readiness"));
    let side = Side {
        nodes: u.nodes.clone(),
        pids: u.pids.clone(),
        source: p.gw_source.clone(),
        probe: if probe { u.nodes.clone() } else { Vec::new() },
        admin: u.admin.clone(),
        counters: def.idle_after_s > 0.0,
    };
    measure::run(ctx, side, load, protocol(ctx, def)).await
}

async fn direct_run(ctx: &Ctx, def: &CellDef, p: &Prepared, load: Load) -> anyhow::Result<Option<Measured>> {
    let Some(src) = &p.direct_source else {
        return Ok(None);
    };
    let side = Side {
        nodes: vec![ctx.mock_addr],
        pids: Vec::new(),
        source: src.clone(),
        probe: Vec::new(),
        admin: Vec::new(),
        counters: false,
    };
    Ok(Some(measure::run(ctx, side, load, protocol(ctx, def)).await?))
}

#[allow(clippy::cast_precision_loss)]
fn peak(m: &Measured) -> Option<f64> {
    m.rss.iter().map(|(_, b)| *b as f64).reduce(f64::max)
}

/// Open, closed and held-stream cells: per repetition a gateway run, then its direct run.
pub(crate) async fn traffic(
    ctx: &Ctx,
    def: &CellDef,
    dir: &Path,
    reps: std::ops::RangeInclusive<u32>,
) -> anyhow::Result<()> {
    let scen_name = def.scenario.as_deref().context("the cell names no scenario")?;
    let scen = source::load(&ctx.paths, &ctx.scenarios, scen_name)?;
    let u = bring_up(ctx, def, Some(&scen), &def.cell).await?;
    let result = async {
        let idle = idle_rss(&u.pids).await;
        let p = prepare(ctx, def, &u).await?;
        let scripted = p.scen.scripted(p.mock);
        let load = load(ctx, def, scripted, None)?;
        for rep in reps {
            let gw = gateway_run(ctx, def, &u, &p, load).await?;
            let direct = if def.kind == Kind::Hold {
                None
            } else {
                direct_run(ctx, def, &p, load).await?
            };
            let mut extra = BTreeMap::new();
            if let Some(i) = idle {
                extra.insert("rss_bytes.idle".to_owned(), i);
            }
            if let (Some(i), Some(pk)) = (idle, peak(&gw)) {
                extra.insert("rss_over_idle_bytes".to_owned(), pk - i);
                if let (Some(c), Kind::Closed) = (def.level.concurrency, def.kind) {
                    #[allow(clippy::cast_precision_loss)]
                    let per = (pk - i) / (c as f64 * p.scen.body_bytes as f64);
                    extra.insert("rss_per_inflight_over_body".to_owned(), per);
                }
            }
            if def.idle_after_s > 0.0 {
                soak_extras(ctx, def, &u, &gw, &mut extra).await;
            }
            let validity = output::validity(ctx, &gw, direct.as_ref(), scripted, false);
            output::write_rep(
                ctx,
                &dir.join(format!("rep-{rep}")),
                &gw,
                direct.as_ref(),
                &extra,
                &validity,
            )?;
            eprintln!(
                "{} rep {rep}: {} ok of {} offered{}",
                def.cell,
                gw.report.counts.ok,
                gw.report.counts.offered,
                if validity.valid {
                    String::new()
                } else {
                    format!(" (invalid: {})", validity.reasons.join("; "))
                }
            );
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    bring_down(ctx, u).await;
    result
}

/// After a soak: idle, then compare memory, descriptors and tasks with the post-warm-up baseline.
async fn soak_extras(ctx: &Ctx, def: &CellDef, u: &Under, gw: &Measured, extra: &mut BTreeMap<String, f64>) {
    tokio::time::sleep(Duration::from_secs_f64(def.idle_after_s)).await;
    #[allow(clippy::cast_precision_loss)]
    let baseline = gw.rss.iter().find(|(t, _)| *t >= 0.0).map(|(_, b)| *b as f64);
    #[allow(clippy::cast_precision_loss)]
    let last = hosts::read_all(&u.pids).await.map(|r| r.rss_bytes as f64);
    if let (Some(b), Some(f)) = (baseline, last) {
        extra.insert("rss_final_bytes".to_owned(), f);
        extra.insert(
            "rss_final_excess_bytes".to_owned(),
            f - (1.10 * b + 16.0 * 1024.0 * 1024.0),
        );
    }
    let growth = |base: Option<f64>, now: Option<f64>| Some((now? - base?) / base?.max(1.0) * 100.0);
    if let Some(g) = growth(gw.fds.0, measure::fds(&u.pids).await) {
        extra.insert("fds_growth_pct".to_owned(), g);
    }
    if let Some(g) = growth(gw.tasks.0, measure::tasks(ctx, &u.admin).await) {
        extra.insert("tasks_growth_pct".to_owned(), g);
    }
}

/// One ladder step passes when the achieved rate is at least 99.5 % of offered, errors at most
/// 0.01 % and the p99 overhead (total latency without a direct run) at most 5 ms.
fn step_passes(gw: &Measured, direct: Option<&Measured>) -> (bool, f64) {
    let c = &gw.report.counts;
    #[allow(clippy::cast_precision_loss)]
    let (achieved, errors) = if c.offered == 0 {
        (0.0, 1.0)
    } else {
        (
            (c.ok + c.disconnects) as f64 / c.offered as f64,
            c.errors() as f64 / c.offered as f64,
        )
    };
    #[allow(clippy::cast_precision_loss)]
    let p99 = gw.report.hist.latency.value_at_quantile(0.99) as f64
        - direct.map_or(0.0, |d| {
            #[allow(clippy::cast_precision_loss)]
            let v = d.report.hist.latency.value_at_quantile(0.99) as f64;
            v
        });
    (achieved >= 0.995 && errors <= 0.0001 && p99 <= 5000.0, p99)
}

/// The saturation ladder: +step until a step fails, then +fine_step from the last passing rate.
pub(crate) async fn saturation(
    ctx: &Ctx,
    def: &CellDef,
    dir: &Path,
    reps: std::ops::RangeInclusive<u32>,
) -> anyhow::Result<()> {
    let ladder = def.level.ladder.unwrap_or_default();
    let scen_name = def.scenario.as_deref().context("the cell names no scenario")?;
    let scen = source::load(&ctx.paths, &ctx.scenarios, scen_name)?;
    let u = bring_up(ctx, def, Some(&scen), &def.cell).await?;
    let result = async {
        let idle = idle_rss(&u.pids).await;
        let p = prepare(ctx, def, &u).await?;
        let scripted = p.scen.scripted(p.mock);
        for rep in reps {
            let mut steps = Vec::new();
            let mut best: Option<(f64, Measured, Option<Measured>)> = None;
            let mut rate = ladder.start;
            let mut fine = false;
            let mut ceiling = ladder.max;
            while rate <= ceiling {
                let l = load(ctx, def, scripted, Some(rate))?;
                let gw = gateway_run(ctx, def, &u, &p, l).await?;
                let direct = direct_run(ctx, def, &p, l).await?;
                let (ok, p99) = step_passes(&gw, direct.as_ref());
                steps.push(
                    json!({"offered_rps": rate, "achieved_rps": gw.report.achieved_rps,
                                  "errors": gw.report.counts.errors(), "overhead_p99_us": p99, "passed": ok}),
                );
                eprintln!(
                    "{} rep {rep}: {rate} RPS {}",
                    def.cell,
                    if ok { "passed" } else { "failed" }
                );
                if ok {
                    best = Some((rate, gw, direct));
                    rate += if fine { ladder.fine_step } else { ladder.step };
                } else if !fine {
                    fine = true;
                    ceiling = rate - ladder.fine_step;
                    rate = best.as_ref().map_or(ladder.start, |b| b.0) + ladder.fine_step;
                    if best.is_none() {
                        break;
                    }
                } else {
                    break;
                }
            }
            let rep_dir = dir.join(format!("rep-{rep}"));
            std::fs::create_dir_all(&rep_dir)?;
            write_json(&rep_dir.join("ladder.json"), &json!({"steps": steps}))?;
            let mut extra = BTreeMap::new();
            if let Some(i) = idle {
                extra.insert("rss_bytes.idle".to_owned(), i);
            }
            match best {
                Some((r, gw, direct)) => {
                    extra.insert("saturation_rps".to_owned(), r);
                    let validity = output::validity(ctx, &gw, direct.as_ref(), scripted, true);
                    output::write_rep(ctx, &rep_dir, &gw, direct.as_ref(), &extra, &validity)?;
                }
                None => {
                    extra.insert("saturation_rps".to_owned(), 0.0);
                    write_json(&rep_dir.join("extra.json"), &extra)?;
                    write_json(
                        &rep_dir.join("validity.json"),
                        &Validity {
                            valid: !ctx.debug_build,
                            reasons: if ctx.debug_build {
                                vec!["the harness was built without optimisations".to_owned()]
                            } else {
                                Vec::new()
                            },
                        },
                    )?;
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    bring_down(ctx, u).await;
    result
}

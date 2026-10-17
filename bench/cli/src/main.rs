//! `antirion-bench`: the benchmark harness. Runs suites and single cells against a topology, checks
//! the mock and the generator before every suite, and renders reports and gates from raw results.

#![forbid(unsafe_code)]

mod agent;
mod cells;
mod config;
mod ctx;
mod gateway;
mod hosts;
mod inventory;
mod kinds;
mod measure;
mod micro;
mod output;
mod procs;
mod profile;
mod sampler;
mod selftest;
mod source;
mod suite;
mod targets;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use antirion_bench_report::compare;
use antirion_bench_report::model::{CellDef, Kind, Level};
use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};

use crate::config::{Paths, Topology, parse_duration};
use crate::ctx::{Target, Windows};
use crate::suite::RunSpec;

#[derive(Debug, Parser)]
#[command(name = "antirion-bench", about = "The Antirion benchmark harness")]
struct Cli {
    /// The bench directory (default: found from the binary or the working directory).
    #[arg(long, global = true)]
    bench_dir: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Mock and load-generator self-tests, and a smoke test of every scenario through the gateway.
    Selftest {
        #[command(flatten)]
        common: Common,
        /// Skip the gateway smoke test.
        #[arg(long)]
        no_gateway: bool,
    },
    /// Reproduce one cell of the matrix, or an ad hoc one.
    Cell(CellArgs),
    /// Run a suite: release, nightly or compare.
    Run {
        #[command(flatten)]
        common: Common,
        #[arg(long)]
        suite: String,
        /// Only these cells (comma-separated ids).
        #[arg(long, value_delimiter = ',')]
        cells: Vec<String>,
        #[arg(long)]
        skip_selftest: bool,
    },
    /// Render summaries, gates and the report of a result directory.
    Report {
        dir: PathBuf,
        /// A previous run: adds the regression gate against it.
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// Other runs whose cells cross-run gates refer to (the ref-4 run for the ref-16 factor).
        #[arg(long)]
        with: Vec<PathBuf>,
    },
    /// Create the topology's machines.
    Provision {
        #[arg(long)]
        topology: String,
    },
    /// Destroy the topology's machines.
    Teardown {
        #[arg(long)]
        topology: String,
    },
    /// Check the committed corpus against its seed, or write it.
    Corpus {
        #[arg(long)]
        write: bool,
    },
    /// List a suite's cells.
    List {
        #[arg(long, default_value = "release")]
        suite: String,
    },
    /// Serve as the agent of this machine: start, sample and stop processes for a harness running
    /// elsewhere (the token is read from ANTIRION_BENCH_AGENT_TOKEN).
    Agent {
        #[arg(long, default_value = "0.0.0.0:19500")]
        listen: SocketAddr,
        /// Where started processes keep their files (default: a temporary directory).
        #[arg(long)]
        work_dir: Option<PathBuf>,
    },
    /// Write the environment carrying a profile's keys, for a gateway started by hand.
    Env {
        #[arg(long)]
        profile: String,
        /// The mock's address as the gateway reaches it.
        #[arg(long)]
        mock: SocketAddr,
        #[arg(long, default_value = "L0")]
        mock_profile: String,
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Debug, Args, Clone)]
struct Common {
    #[arg(long)]
    topology: String,
    /// `antirion`, or `external` for another gateway at --target-url.
    #[arg(long, default_value = "antirion")]
    target: String,
    /// A name for an external target, used in result paths.
    #[arg(long)]
    target_name: Option<String>,
    /// Data-plane address(es) of an external target (host:port).
    #[arg(long)]
    target_url: Vec<String>,
    /// The key requests to an external target carry.
    #[arg(long)]
    target_key: Option<String>,
    /// The header carrying the key (`authorization` sends it with the Bearer scheme).
    #[arg(long, default_value = targets::DEFAULT_AUTH_HEADER)]
    target_auth_header: String,
    /// The model an external target routes to the mock: `MODEL`, or `PROFILE=MODEL` for one latency
    /// profile (repeatable).
    #[arg(long)]
    target_model: Vec<String>,
    /// Pids of a local external target, for memory and CPU.
    #[arg(long)]
    target_pid: Vec<u32>,
    /// A pattern of a local external target's command lines (`pgrep -f`), for memory and CPU; the
    /// processes below the matches count too.
    #[arg(long)]
    target_process: Option<String>,
    /// The external target's version, for the manifest.
    #[arg(long)]
    target_version: Option<String>,
    /// The agent (`host:port`) of the machine an external target runs on, for its memory and CPU.
    #[arg(long)]
    target_agent: Option<String>,
    #[arg(long)]
    gateway_bin: Option<PathBuf>,
    /// Result directory (default: bench/results/<UTC>-<suite>-<topology>-<target>).
    #[arg(long)]
    out: Option<PathBuf>,
    /// Data directories and logs of the processes (default: a temporary directory).
    #[arg(long)]
    work_dir: Option<PathBuf>,
    /// Override the 30 s warm-up (not publishable).
    #[arg(long)]
    warmup: Option<String>,
    /// Override the 120 s measurement (not publishable).
    #[arg(long)]
    measure: Option<String>,
    /// Override the repetitions (not publishable).
    #[arg(long)]
    reps: Option<u32>,
}

#[derive(Debug, Args)]
struct CellArgs {
    #[command(flatten)]
    common: Common,
    /// A cell of the matrix.
    #[arg(long, conflicts_with_all = ["scenario", "cell_file"])]
    cell: Option<String>,
    /// Cells defined in a file, in the matrix's form (one cell or a list).
    #[arg(long, conflicts_with_all = ["scenario"])]
    cell_file: Option<PathBuf>,
    #[arg(long)]
    scenario: Option<String>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    rps: Option<f64>,
    #[arg(long)]
    concurrency: Option<usize>,
    #[arg(long)]
    streams: Option<u64>,
    #[arg(long, default_value = "L0")]
    mock: String,
    #[arg(long, default_value = "h1")]
    protocol: String,
    /// An id for the ad hoc cell.
    #[arg(long, default_value = "adhoc")]
    id: String,
    /// Run the cells for every target of this file, interleaved per cell and repetition; each
    /// target's results go to `<out>/<name>`, the comparison to `<out>/comparison.md`.
    #[arg(long)]
    interleave: Option<PathBuf>,
    #[arg(long)]
    skip_selftest: bool,
}

fn target(c: &Common) -> anyhow::Result<Target> {
    match c.target.as_str() {
        "antirion" => Ok(Target::Antirion),
        "external" => {
            let nodes = c
                .target_url
                .iter()
                .map(|u| targets::parse_node(u))
                .collect::<anyhow::Result<Vec<_>>>()?;
            if nodes.is_empty() {
                bail!("an external target needs --target-url");
            }
            Ok(Target::External(Box::new(targets::External {
                name: c.target_name.clone().unwrap_or_else(|| "external".to_owned()),
                nodes,
                key: c
                    .target_key
                    .clone()
                    .context("an external target needs --target-key")?,
                auth_header: c.target_auth_header.clone(),
                models: targets::parse_models(&c.target_model)?,
                pids: c.target_pid.clone(),
                agent: c.target_agent.as_deref().map(targets::parse_node).transpose()?,
                process: c.target_process.clone(),
                version: c.target_version.clone(),
                start: None,
                statuses: std::collections::BTreeMap::new(),
            })))
        }
        other => bail!("unknown target {other} (antirion, external)"),
    }
}

fn spec(c: &Common, suite: &str) -> anyhow::Result<RunSpec> {
    spec_for(c, suite, target(c)?)
}

fn spec_for(c: &Common, suite: &str, target: Target) -> anyhow::Result<RunSpec> {
    Ok(RunSpec {
        suite: suite.to_owned(),
        topology: c.topology.clone(),
        target,
        gateway_bin: c.gateway_bin.clone(),
        out: c.out.clone(),
        work: c.work_dir.clone(),
        windows: Windows {
            warmup: c.warmup.as_deref().map(parse_duration).transpose()?,
            measure: c.measure.as_deref().map(parse_duration).transpose()?,
            reps: c.reps,
        },
        command: std::env::args().collect(),
    })
}

fn summarize(out: &std::path::Path, o: &antirion_bench_report::Outcome) {
    println!(
        "{} cells ({} invalid); gates: {} pass, {} fail, {} invalid, {} n/a",
        o.cells,
        o.invalid_cells,
        o.count("pass"),
        o.count("fail"),
        o.count("invalid"),
        o.count("n/a")
    );
    println!("report: {}", out.join("report.md").display());
}

fn adhoc(a: &CellArgs) -> anyhow::Result<CellDef> {
    let kind = if a.concurrency.is_some() {
        Kind::Closed
    } else {
        Kind::Open
    };
    if a.rps.is_none() && a.concurrency.is_none() && a.streams.is_none() {
        bail!("an ad hoc cell needs --rps, --concurrency or --streams");
    }
    let mut def: CellDef = serde_json::from_value(serde_json::json!({
        "cell": a.id, "kind": "open", "informational": true, "compare": true,
    }))?;
    def.kind = kind;
    def.scenario = Some(a.scenario.clone().context("--scenario or --cell")?);
    def.profile = Some(a.profile.clone().unwrap_or_else(|| "P-min".to_owned()));
    def.mock_profile = Some(a.mock.clone());
    def.protocol.clone_from(&a.protocol);
    def.level = Level {
        rps: a.rps,
        concurrent_streams: a.streams,
        concurrency: a.concurrency,
        ..Level::default()
    };
    Ok(def)
}

async fn main_async(cli: Cli) -> anyhow::Result<bool> {
    if let Cmd::Agent { listen, work_dir } = cli.cmd {
        let work = work_dir.unwrap_or_else(|| std::env::temp_dir().join("antirion-bench-agent"));
        agent::serve(listen, work).await?;
        return Ok(true);
    }
    let paths = Paths::locate(cli.bench_dir.as_deref())?;
    match cli.cmd {
        Cmd::Agent { .. } => Ok(true),
        Cmd::Selftest { common, no_gateway } => {
            let spec = spec(&common, "selftest")?;
            let ctx = suite::open(paths, &spec).await?;
            let passed = selftest::run(&ctx, !no_gateway).await;
            let _ = suite::close(&ctx, &spec).await;
            let passed = passed?;
            println!(
                "selftest {}: {}",
                if passed { "passed" } else { "FAILED" },
                ctx.out.join("selftest.json").display()
            );
            Ok(passed)
        }
        Cmd::Cell(a) => {
            let defs: Vec<CellDef> = match (&a.cell, &a.cell_file) {
                (Some(id), _) => vec![
                    config::cells(&paths)?
                        .into_iter()
                        .find(|c| &c.cell == id)
                        .with_context(|| format!("no cell {id} in the matrix"))?,
                ],
                (None, Some(file)) => match config::read_yaml::<Vec<CellDef>>(file) {
                    Ok(list) => list,
                    Err(_) => vec![config::read_yaml(file)?],
                },
                (None, None) => vec![adhoc(&a)?],
            };
            if let Some(file) = &a.interleave {
                return Box::pin(interleaved(paths, &a, file, &defs)).await;
            }
            let spec = spec(&a.common, "cell")?;
            let ctx = suite::open(paths, &spec).await?;
            let r = async {
                if !a.skip_selftest {
                    suite::selftest_or_abort(&ctx, false).await?;
                }
                suite::run_cells(&ctx, &defs).await
            }
            .await;
            let outcome = suite::close(&ctx, &spec).await?;
            r?;
            summarize(&ctx.out, &outcome);
            Ok(true)
        }
        Cmd::Run {
            common,
            suite: name,
            cells,
            skip_selftest,
        } => {
            if !["release", "nightly", "compare"].contains(&name.as_str()) {
                bail!("unknown suite {name} (release, nightly, compare)");
            }
            let mut list = config::suite(&config::cells(&paths)?, &name);
            if !cells.is_empty() {
                list.retain(|c| cells.contains(&c.cell));
            }
            let spec = spec(&common, &name)?;
            let ctx = suite::open(paths, &spec).await?;
            let r = async {
                if !skip_selftest {
                    suite::selftest_or_abort(&ctx, matches!(ctx.target, Target::Antirion)).await?;
                }
                suite::run_cells(&ctx, &list).await
            }
            .await;
            let outcome = suite::close(&ctx, &spec).await?;
            r?;
            summarize(&ctx.out, &outcome);
            Ok(true)
        }
        Cmd::Report { dir, baseline, with } => {
            if !dir.join("run.json").exists() && dir.join(compare::TARGETS_FILE).exists() {
                let c = compare::render(&dir)?;
                println!(
                    "{} targets, {} cells; comparison: {}",
                    c.targets,
                    c.cells,
                    dir.join("comparison.md").display()
                );
                return Ok(true);
            }
            let o = antirion_bench_report::render(&dir, baseline.as_deref(), &with)?;
            summarize(&dir, &o);
            Ok(true)
        }
        Cmd::Provision { topology } | Cmd::Teardown { topology } => {
            let t = Topology::load(&paths, &topology)?;
            bail!(
                "provisioning is not supported for topology {}: create the machines yourself, start the \
                 gateway nodes and the mock on them, and list their addresses in {}",
                t.name,
                t.file.display()
            )
        }
        Cmd::Corpus { write } => {
            if write {
                antirion_bench_corpus::write(&paths.corpus(), antirion_bench_corpus::SEED)?;
                println!("wrote {}", paths.corpus().display());
            } else {
                antirion_bench_corpus::check(&paths.corpus())?;
                println!(
                    "the committed corpus matches seed {}",
                    antirion_bench_corpus::SEED
                );
            }
            Ok(true)
        }
        Cmd::List { suite: name } => {
            for c in config::suite(&config::cells(&paths)?, &name) {
                println!(
                    "{:<20} {:<10} {:<16} {:<10} {}",
                    c.cell,
                    format!("{:?}", c.kind).to_lowercase(),
                    c.scenario.unwrap_or_default(),
                    c.profile.unwrap_or_default(),
                    c.needs_topology.map(|t| format!("needs {t}")).unwrap_or_default()
                );
            }
            Ok(true)
        }
        Cmd::Env {
            profile,
            mock,
            mock_profile,
            out,
        } => {
            let p: antirion_bench_mock::Profile = mock_profile.parse()?;
            let endpoints = profile::Endpoints {
                mock: format!("http://{mock}/{}", p.name()),
                sink: format!("http://{mock}/__bench/null"),
            };
            let r = profile::render(&paths, &profile, &endpoints, antirion_bench_corpus::SEED)?;
            gateway::write_env(&out, &r)?;
            let doc = out.with_extension("config.json");
            std::fs::write(&doc, serde_json::to_vec_pretty(&r.doc)?)?;
            println!(
                "wrote {} keys to {} and the document to {}",
                r.env.len(),
                out.display(),
                doc.display()
            );
            Ok(true)
        }
    }
}

/// `cell --interleave`: one run per target, sharing the mock, cells interleaved across them.
async fn interleaved(
    paths: Paths,
    a: &CellArgs,
    file: &std::path::Path,
    defs: &[CellDef],
) -> anyhow::Result<bool> {
    let list = targets::load(file)?;
    let stamp = jiff::Timestamp::now().strftime("%Y%m%dT%H%M%SZ").to_string();
    let root = a
        .common
        .out
        .clone()
        .unwrap_or_else(|| paths.bench.join("results").join(format!("{stamp}-interleave")));
    std::fs::create_dir_all(&root)?;
    let work = a
        .common
        .work_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("antirion-bench-{}", std::process::id())));
    let names: Vec<String> = list.iter().map(|t| t.name().to_owned()).collect();
    antirion_bench_report::model::write_json(
        &root.join(compare::TARGETS_FILE),
        &serde_json::json!({"targets": names, "cells": defs.iter().map(|d| d.cell.clone()).collect::<Vec<_>>()}),
    )?;
    let mut ctxs: Vec<ctx::Ctx> = Vec::new();
    let opened = async {
        for t in list {
            let mut common = a.common.clone();
            common.out = Some(root.join(t.name()));
            common.work_dir = Some(work.join(t.name()));
            let spec = spec_for(&common, "cell", t)?;
            let ctx = suite::open_with(paths.clone(), &spec, ctxs.first()).await?;
            ctxs.push(ctx);
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let r = async {
        opened?;
        if !a.skip_selftest
            && let Some(first) = ctxs.first()
        {
            suite::selftest_or_abort(first, false).await?;
        }
        suite::interleave(&ctxs, defs).await
    }
    .await;
    for ctx in ctxs.iter().rev() {
        let mut common = a.common.clone();
        common.out = Some(ctx.out.clone());
        let spec = spec_for(&common, "cell", ctx.target.clone())?;
        let o = suite::close(ctx, &spec).await?;
        println!(
            "{}: {} cells ({} invalid)",
            ctx.target.name(),
            o.cells,
            o.invalid_cells
        );
    }
    let _ = std::fs::remove_dir_all(&work);
    r?;
    let c = compare::render(&root)?;
    println!(
        "{} targets, {} cells; comparison: {}",
        c.targets,
        c.cells,
        root.join("comparison.md").display()
    );
    Ok(true)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build();
    let result = match rt {
        Ok(rt) => rt.block_on(main_async(cli)),
        Err(e) => Err(e.into()),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("antirion-bench: {e:#}");
            ExitCode::FAILURE
        }
    }
}

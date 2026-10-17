//! Cells without traffic: idle memory, startup time, and the time a one-field apply takes to reach
//! every node.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use antirion_bench_loadgen::fetch;
use antirion_bench_report::model::{CellDef, Kind, Validity, write_json};
use anyhow::{Context, bail};
use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use tokio_util::sync::CancellationToken;

use crate::cells::{Under, bring_down, bring_up};
use crate::ctx::Ctx;
use crate::gateway;
use crate::hosts::Pid;
use crate::sampler;

/// Samples memory for `secs` seconds; writes `rss.csv`; the mean.
async fn idle_window(pids: &[Pid], dir: &Path, secs: u64) -> anyhow::Result<Option<f64>> {
    let stop = CancellationToken::new();
    let start = tokio::time::Instant::now();
    let series = sampler::rss_series(pids.to_vec(), start, stop.clone());
    let timer = async {
        tokio::time::sleep(Duration::from_secs(secs)).await;
        stop.cancel();
    };
    let (series, ()) = tokio::join!(series, timer);
    let mut csv = String::from("t_s,rss_bytes\n");
    for (t, b) in &series {
        csv.push_str(&format!("{t:.1},{b}\n"));
    }
    std::fs::write(dir.join("rss.csv"), csv)?;
    #[allow(clippy::cast_precision_loss)]
    let mean = (!series.is_empty())
        .then(|| series.iter().map(|(_, b)| *b as f64).sum::<f64>() / series.len() as f64);
    Ok(mean)
}

fn validity(ctx: &Ctx) -> Validity {
    let reasons: Vec<String> = if ctx.debug_build {
        vec!["the harness was built without optimisations".to_owned()]
    } else {
        Vec::new()
    };
    Validity {
        valid: reasons.is_empty(),
        reasons,
    }
}

fn write(ctx: &Ctx, dir: &Path, extra: &BTreeMap<String, f64>) -> anyhow::Result<()> {
    write_json(&dir.join("extra.json"), extra)?;
    write_json(&dir.join("validity.json"), &validity(ctx))?;
    Ok(())
}

pub(crate) async fn run(
    ctx: &Ctx,
    def: &CellDef,
    dir: &Path,
    reps: std::ops::RangeInclusive<u32>,
) -> anyhow::Result<()> {
    match def.kind {
        Kind::Apply => apply(ctx, def, dir, reps).await,
        _ => {
            for rep in reps {
                let rep_dir = dir.join(format!("rep-{rep}"));
                std::fs::create_dir_all(&rep_dir)?;
                let u = bring_up(ctx, def, None, &format!("{}-{rep}", def.cell)).await?;
                let r = once(ctx, def, &u, &rep_dir).await;
                bring_down(ctx, u).await;
                r?;
            }
            Ok(())
        }
    }
}

async fn once(ctx: &Ctx, def: &CellDef, u: &Under, dir: &Path) -> anyhow::Result<()> {
    let mut extra = BTreeMap::new();
    if def.kind == Kind::Startup {
        let ready = u.gw.as_ref().context("no gateway was started")?.ready;
        extra.insert("ready_ms".to_owned(), ready.as_secs_f64() * 1000.0);
        eprintln!("{}: ready after {:.0} ms", def.cell, ready.as_secs_f64() * 1000.0);
    }
    tokio::time::sleep(Duration::from_secs(5)).await;
    if let Some(mean) = idle_window(&u.pids, dir, 10).await? {
        extra.insert("rss_bytes.idle".to_owned(), mean);
    }
    write(ctx, dir, &extra)
}

/// Merge-patches one model's description and waits for every node to serve the new version.
async fn apply_once(ctx: &Ctx, u: &Under, n: u32) -> anyhow::Result<Duration> {
    let admin = *u.admin.first().context("no admin listener")?;
    let token = ctx.admin_token_for()?;
    let body = serde_json::json!({"spec": {"description": format!("apply {n}")}}).to_string();
    let req = Request::patch("/admin/v1/models/bench-chat")
        .header("host", admin.to_string())
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/merge-patch+json")
        .body(Full::new(Bytes::from(body)))?;
    let started = Instant::now();
    let (status, headers, text) = fetch(admin, req, Duration::from_secs(60)).await?;
    if !status.is_success() {
        bail!("the apply answered {status}: {}", String::from_utf8_lossy(&text));
    }
    let generation: u64 = headers
        .get("x-antirion-config-generation")
        .and_then(|v| v.to_str().ok()?.parse().ok())
        .context("the apply returned no configuration generation")?;
    for node in &u.nodes {
        loop {
            if gateway::config_version(*node)
                .await
                .is_some_and(|v| v >= generation)
            {
                break;
            }
            if started.elapsed() > Duration::from_secs(30) {
                bail!("{node} did not serve generation {generation} within 30 s");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    Ok(started.elapsed())
}

async fn apply(
    ctx: &Ctx,
    def: &CellDef,
    dir: &Path,
    reps: std::ops::RangeInclusive<u32>,
) -> anyhow::Result<()> {
    let u = bring_up(ctx, def, None, &def.cell).await?;
    let result = async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let first = dir.join("rep-1");
        std::fs::create_dir_all(&first)?;
        let idle = idle_window(&u.pids, &first, 10).await?;
        for rep in reps {
            let rep_dir = dir.join(format!("rep-{rep}"));
            std::fs::create_dir_all(&rep_dir)?;
            let took = apply_once(ctx, &u, rep).await?;
            let mut extra = BTreeMap::new();
            extra.insert("apply_ms".to_owned(), took.as_secs_f64() * 1000.0);
            if let Some(i) = idle {
                extra.insert("rss_bytes.idle".to_owned(), i);
            }
            eprintln!(
                "{} rep {rep}: applied everywhere in {:.0} ms",
                def.cell,
                took.as_secs_f64() * 1000.0
            );
            write(ctx, &rep_dir, &extra)?;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    bring_down(ctx, u).await;
    result
}

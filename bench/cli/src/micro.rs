//! Micro-benchmark cells: criterion benchmarks of the gateway's crates, run in the repository, and
//! their per-iteration distribution read back from criterion's raw samples.

use std::collections::BTreeMap;
use std::path::Path;

use antirion_bench_report::model::{CellDef, Validity, write_json};
use anyhow::{Context, bail};
use serde::Deserialize;

use crate::ctx::Ctx;

#[derive(Debug, Deserialize)]
struct Sample {
    iters: Vec<f64>,
    times: Vec<f64>,
}

/// Per-iteration nanoseconds of every sample, sorted.
fn per_iteration(s: &Sample) -> Vec<f64> {
    let mut v: Vec<f64> = s
        .iters
        .iter()
        .zip(&s.times)
        .filter(|(i, _)| **i > 0.0)
        .map(|(i, t)| t / i)
        .collect();
    v.sort_by(f64::total_cmp);
    v
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let i = ((sorted.len() as f64 - 1.0) * q).round() as usize;
    sorted.get(i).copied().unwrap_or(f64::NAN)
}

/// `<group>.<function>.{p50,p99,mean}_ns` for every benchmark under `criterion/<group>/`.
pub(crate) fn read_group(criterion: &Path, group: &str) -> anyhow::Result<BTreeMap<String, f64>> {
    let mut out = BTreeMap::new();
    let dir = criterion.join(group);
    let entries =
        std::fs::read_dir(&dir).with_context(|| format!("no criterion results in {}", dir.display()))?;
    for e in entries {
        let path = e?.path();
        let sample = path.join("new").join("sample.json");
        if !sample.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        let s: Sample = serde_json::from_str(&std::fs::read_to_string(&sample)?)?;
        let v = per_iteration(&s);
        #[allow(clippy::cast_precision_loss)]
        let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
        out.insert(format!("{group}.{name}.p50_ns"), quantile(&v, 0.5));
        out.insert(format!("{group}.{name}.p99_ns"), quantile(&v, 0.99));
        out.insert(format!("{group}.{name}.mean_ns"), mean);
    }
    if out.is_empty() {
        bail!("criterion wrote no samples under {}", dir.display());
    }
    Ok(out)
}

/// Runs `cargo bench -p <package> --bench <bench>` in the repository and records the samples.
pub(crate) async fn run(ctx: &Ctx, def: &CellDef, dir: &Path) -> anyhow::Result<()> {
    let spec = def.bench.as_deref().context("a micro cell names its bench")?;
    let (package, bench) = spec.split_once('/').context("bench is <package>/<bench>")?;
    let argv: Vec<String> = [
        "cargo", "bench", "--locked", "-p", package, "--bench", bench, "--", "--noplot",
    ]
    .map(str::to_owned)
    .to_vec();
    ctx.record_command(&argv);
    let log = std::fs::File::create(dir.join("cargo-bench.log"))?;
    let status = tokio::process::Command::new("cargo")
        .args(argv.iter().skip(1))
        .current_dir(&ctx.paths.repo)
        .stdout(log.try_clone()?)
        .stderr(log)
        .status()
        .await
        .context("cannot run cargo")?;
    if !status.success() {
        bail!("cargo bench failed ({status}); see cargo-bench.log");
    }
    let target =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| ctx.paths.repo.join("target"), Into::into);
    let extra = read_group(&target.join("criterion"), bench)?;
    let rep = dir.join("rep-1");
    std::fs::create_dir_all(&rep)?;
    write_json(&rep.join("extra.json"), &extra)?;
    write_json(
        &rep.join("validity.json"),
        &Validity {
            valid: true,
            reasons: Vec::new(),
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_criterion_samples() {
        let dir = std::env::temp_dir().join(format!("crit-{}", std::process::id()));
        let f = dir.join("grp").join("open").join("new");
        std::fs::create_dir_all(&f).unwrap();
        let iters: Vec<f64> = (1..=100).map(f64::from).collect();
        let times: Vec<f64> = iters.iter().map(|i| i * 1000.0).collect();
        std::fs::write(
            f.join("sample.json"),
            serde_json::json!({"sampling_mode": "Linear", "iters": iters, "times": times}).to_string(),
        )
        .unwrap();
        let m = read_group(&dir, "grp").unwrap();
        assert!((m["grp.open.p99_ns"] - 1000.0).abs() < 1e-6);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

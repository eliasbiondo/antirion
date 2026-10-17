//! Writing a repetition's files, and the validity rules of a run.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use antirion_bench_loadgen::Counts;
use antirion_bench_report::hdr;
use antirion_bench_report::model::{CountsFile, CpuFile, Validity, write_json};

use crate::ctx::Ctx;
use crate::measure::Measured;

fn counts_file(c: &Counts, achieved: f64, connections: u64) -> CountsFile {
    CountsFile {
        offered: c.offered,
        achieved_rps: achieved,
        ok: c.ok,
        non_2xx: c.non_2xx.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        connect_errors: c.connect_errors,
        timeouts: c.timeouts,
        body_errors: c.body_errors,
        disconnects: c.disconnects,
        held: c.held,
        late: c.late,
        connections,
    }
}

fn write_hdr(
    ctx: &Ctx,
    dir: &Path,
    name: &str,
    m: &Measured,
    h: &hdrhistogram::Histogram<u64>,
) -> anyhow::Result<()> {
    if h.is_empty() {
        return Ok(());
    }
    let w = &m.report.window;
    hdr::write(
        &dir.join(name),
        h,
        ctx.wall(w.start),
        w.end.saturating_duration_since(w.start),
    )?;
    Ok(())
}

/// Writes `rep-<n>/` for a gateway run and its direct run.
pub(crate) fn write_rep(
    ctx: &Ctx,
    dir: &Path,
    gw: &Measured,
    direct: Option<&Measured>,
    extra: &BTreeMap<String, f64>,
    validity: &Validity,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let h = &gw.report.hist;
    write_hdr(ctx, dir, "gw.hdr", gw, &h.latency)?;
    write_hdr(ctx, dir, "ttft-gw.hdr", gw, &h.ttft)?;
    write_hdr(ctx, dir, "gap-gw.hdr", gw, &h.gap)?;
    write_hdr(ctx, dir, "overhead-header.hdr", gw, &h.overhead_header)?;
    write_hdr(ctx, dir, "lag-gw.hdr", gw, &h.lag)?;
    if let Some(d) = direct {
        let dh = &d.report.hist;
        write_hdr(ctx, dir, "direct.hdr", d, &dh.latency)?;
        write_hdr(ctx, dir, "ttft-direct.hdr", d, &dh.ttft)?;
        write_hdr(ctx, dir, "gap-direct.hdr", d, &dh.gap)?;
        write_json(
            &dir.join("counts-direct.json"),
            &counts_file(&d.report.counts, d.report.achieved_rps, d.report.connections),
        )?;
    }
    let mut csv = String::from("t_s,rss_bytes\n");
    for (t, b) in &gw.rss {
        csv.push_str(&format!("{t:.1},{b}\n"));
    }
    std::fs::write(dir.join("rss.csv"), csv)?;
    let generator = direct.map_or(gw.generator_cpu_pct, |d| {
        d.generator_cpu_pct.max(gw.generator_cpu_pct)
    });
    let mock = match (gw.mock_cpu_pct, direct.and_then(|d| d.mock_cpu_pct)) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    write_json(
        &dir.join("cpu.json"),
        &CpuFile {
            gateway_cpu_s: gw.gateway_cpu_s,
            generator_cpu_pct: generator,
            mock_cpu_pct: mock,
            completed: gw.report.counts.ok,
            gateway_cpu_source: gw.cpu_source.clone(),
        },
    )?;
    write_json(
        &dir.join("counts.json"),
        &counts_file(&gw.report.counts, gw.report.achieved_rps, gw.report.connections),
    )?;
    let mut extra = extra.clone();
    if let Some(u) = gw.upstream_requests {
        extra.insert("upstream_requests".into(), u);
    }
    if let Some(f) = gw.readiness_failures {
        extra.insert("readiness_failures".into(), f);
    }
    write_json(&dir.join("extra.json"), &extra)?;
    write_json(&dir.join("validity.json"), validity)?;
    Ok(())
}

/// The generator and the mock above this share of the machine's CPU invalidate a run.
const CPU_LIMIT_PCT: f64 = 60.0;
/// More than this share of requests sent over 1 ms late invalidates a run.
const LATE_LIMIT: f64 = 0.001;

fn late_fraction(c: &Counts) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    if c.offered == 0 {
        0.0
    } else {
        c.late as f64 / c.offered as f64
    }
}

/// The validity rules of the procedure.
pub(crate) fn validity(
    ctx: &Ctx,
    gw: &Measured,
    direct: Option<&Measured>,
    scripted: Duration,
    ladder: bool,
) -> Validity {
    let mut reasons = Vec::new();
    if ctx.debug_build {
        reasons.push("the harness was built without optimisations".to_owned());
    }
    if gw.report.counts.offered == 0 {
        reasons.push("no request fell in the measurement window".to_owned());
    }
    for (side, m) in std::iter::once(("gateway", gw)).chain(direct.map(|d| ("direct", d))) {
        if m.generator_cpu_pct > CPU_LIMIT_PCT {
            reasons.push(format!(
                "generator at {:.0} % CPU in the {side} run",
                m.generator_cpu_pct
            ));
        }
        if let Some(p) = m.mock_cpu_pct.filter(|p| *p > CPU_LIMIT_PCT) {
            reasons.push(format!("mock at {p:.0} % CPU in the {side} run"));
        }
        let late = late_fraction(&m.report.counts);
        if late > LATE_LIMIT {
            reasons.push(format!(
                "{:.2} % of {side} requests left over 1 ms late",
                late * 100.0
            ));
        }
    }
    if let Some(d) = direct {
        let p99 = Duration::from_micros(d.report.hist.latency.value_at_quantile(0.99));
        let bound = scripted + Duration::from_millis(1);
        if !d.report.hist.latency.is_empty() && p99 > bound {
            reasons.push(format!(
                "direct p99 {p99:?} above the scripted delay + 1 ms ({bound:?})"
            ));
        }
        let c = &d.report.counts;
        #[allow(clippy::cast_precision_loss)]
        let achieved = if c.offered == 0 {
            0.0
        } else {
            (c.ok + c.disconnects) as f64 / c.offered as f64
        };
        if !ladder && achieved < 0.995 {
            reasons.push(format!(
                "the direct run achieved {:.2} % of offered",
                achieved * 100.0
            ));
        }
    }
    Validity {
        valid: reasons.is_empty(),
        reasons,
    }
}

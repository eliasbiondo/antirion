//! The comparison of an interleaved run: one result directory per target under a common root, and
//! `comparison.md`, `comparison.html` and `comparison.json` with every target's cells side by side.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::gates::Summaries;
use crate::model::{CellDef, RunManifest, read_json, write_json};
use crate::render::{Block, Cell, fmt_num, level_text, linked, plain, to_html, to_markdown};
use crate::summary::Summary;
use crate::{ReportError, summaries};

/// The file at the root of an interleaved run naming its targets (in their order) and cells.
pub const TARGETS_FILE: &str = "targets.json";

/// What [`render`] found.
#[derive(Debug, Clone, Copy, Default)]
pub struct Compared {
    pub targets: usize,
    pub cells: usize,
}

#[derive(Debug, Deserialize)]
struct TargetsFile {
    targets: Vec<String>,
    #[serde(default)]
    cells: Vec<String>,
}

/// The latency a cell is compared by: the overhead over the direct run when there is one, the
/// stream's first-token overhead for streams, the total latency when the gateway answers alone.
fn family(present: &dyn Fn(&str) -> bool) -> (&'static str, &'static str) {
    if present("overhead_us.p50") {
        ("overhead", "overhead_us")
    } else if present("ttft_overhead_us.p50") {
        ("TTFT overhead", "ttft_overhead_us")
    } else if present("latency_us.p50") {
        ("latency", "latency_us")
    } else {
        ("—", "latency_us")
    }
}

const MIB: f64 = 1024.0 * 1024.0;

struct Target {
    name: String,
    run: RunManifest,
    cells: Summaries,
}

fn number(v: Option<f64>) -> Value {
    v.filter(|x| x.is_finite())
        .map_or(Value::Null, |x| json!((x * 1000.0).round() / 1000.0))
}

/// Renders every target's own report, then the comparison at `dir`.
pub fn render(dir: &Path) -> Result<Compared, ReportError> {
    let tf: TargetsFile = read_json(&dir.join(TARGETS_FILE))?;
    let mut runs = Vec::new();
    for name in &tf.targets {
        let d = dir.join(name);
        if !d.join("run.json").exists() {
            continue;
        }
        crate::render(&d, None, &[])?;
        runs.push(Target {
            name: name.clone(),
            run: read_json(&d.join("run.json"))?,
            cells: summaries(&d)?,
        });
    }
    let mut order: Vec<String> = tf.cells.clone();
    for t in &runs {
        for id in t.cells.keys() {
            if !order.contains(id) {
                order.push(id.clone());
            }
        }
    }
    order.retain(|id| runs.iter().any(|t| t.cells.contains_key(id)));

    let mut blocks = vec![Block::Heading(1, "Comparison".to_owned())];
    blocks.push(Block::Para(
        "Every target ran the same cells on the same machine, interleaved per cell and repetition \
         (each target in turn on a fresh process). Numbers are medians over the repetitions; each \
         links to the target's `summary.json`, which also holds min and max."
            .to_owned(),
    ));
    let mut notes: Vec<String> = runs
        .iter()
        .flat_map(|t| {
            t.run
                .not_publishable
                .iter()
                .map(move |n| format!("{}: {n}", t.name))
        })
        .collect();
    notes.dedup();
    if !notes.is_empty() {
        blocks.push(Block::Para(format!("**Not publishable:** {}.", notes.join("; "))));
    }
    blocks.push(Block::Table(
        [
            "Target", "Version", "Commit", "Topology", "Started", "Ended", "Report",
        ]
        .map(str::to_owned)
        .to_vec(),
        runs.iter()
            .map(|t| {
                vec![
                    plain(t.name.clone()),
                    plain(t.run.target_version.clone()),
                    plain(t.run.target_commit.clone()),
                    plain(t.run.topology.clone()),
                    plain(t.run.started_at.clone()),
                    plain(t.run.ended_at.clone()),
                    linked("report.md", &format!("{}/report.md", t.name)),
                ]
            })
            .collect(),
    ));

    let headers = [
        "Cell",
        "Load",
        "Target",
        "Latency (µs)",
        "p50",
        "p99",
        "p99.9",
        "Inter-event p99",
        "Achieved RPS",
        "Saturation RPS",
        "Errors",
        "CPU µs/req",
        "RSS idle MiB",
        "RSS peak MiB",
        "Valid",
    ]
    .map(str::to_owned)
    .to_vec();
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    let mut json_cells = Vec::new();
    for id in &order {
        let present = |k: &str| {
            runs.iter()
                .any(|t| t.cells.get(id).is_some_and(|(_, s)| s.stats.contains_key(k)))
        };
        let (label, prefix) = family(&present);
        let def: Option<&CellDef> = runs.iter().find_map(|t| t.cells.get(id).map(|(d, _)| d));
        let load = def.map_or_else(String::new, |d| {
            let mut l = format!("{} {}", d.scenario.clone().unwrap_or_default(), level_text(d));
            if let Some(m) = &d.mock_profile {
                l.push_str(&format!(" ({m})"));
            }
            l
        });
        let mut per_target = serde_json::Map::new();
        for t in &runs {
            let Some((_, s)) = t.cells.get(id) else { continue };
            let link = format!("{}/cells/{id}/summary.json", t.name);
            let med = |k: &str| s.median(k);
            let cell = |v: Option<f64>, scale: f64| match v {
                Some(x) if x.is_finite() => linked(fmt_num(x / scale), &link),
                _ => plain("—"),
            };
            let errors = med("error_rate").map(|e| e * 100.0);
            let valid = validity(s);
            if !s.valid {
                problems.push(format!("{id} on {}: {}", t.name, s.reasons.join("; ")));
            }
            rows.push(vec![
                plain(id.clone()),
                plain(load.clone()),
                plain(t.name.clone()),
                plain(label),
                cell(med(&format!("{prefix}.p50")), 1.0),
                cell(med(&format!("{prefix}.p99")), 1.0),
                cell(med(&format!("{prefix}.p99_9")), 1.0),
                cell(med("inter_event_overhead_us.p99"), 1.0),
                cell(med("achieved_rps"), 1.0),
                cell(med("saturation_rps"), 1.0),
                errors.map_or_else(|| plain("—"), |e| linked(format!("{} %", fmt_num(e)), &link)),
                cell(med("cpu_us_per_request"), 1.0),
                cell(med("rss_bytes.idle"), MIB),
                cell(med("rss_bytes.peak"), MIB),
                plain(valid.clone()),
            ]);
            per_target.insert(
                t.name.clone(),
                json!({
                    "metric": prefix,
                    "p50_us": number(med(&format!("{prefix}.p50"))),
                    "p99_us": number(med(&format!("{prefix}.p99"))),
                    "p99_9_us": number(med(&format!("{prefix}.p99_9"))),
                    "inter_event_p99_us": number(med("inter_event_overhead_us.p99")),
                    "achieved_rps": number(med("achieved_rps")),
                    "saturation_rps": number(med("saturation_rps")),
                    "error_rate": number(med("error_rate")),
                    "cpu_us_per_request": number(med("cpu_us_per_request")),
                    "rss_idle_bytes": number(med("rss_bytes.idle")),
                    "rss_peak_bytes": number(med("rss_bytes.peak")),
                    "repetitions": s.repetitions,
                    "valid": s.valid,
                    "skipped": s.skipped,
                    "reasons": s.reasons,
                }),
            );
        }
        json_cells.push(json!({"cell": id, "load": load, "targets": per_target}));
    }
    blocks.push(Block::Heading(2, "Cells".to_owned()));
    blocks.push(Block::Para(
        "Latency is the gateway's overhead over the direct run to the mock (TTFT overhead for \
         streams), or its total latency where it answers without an upstream call (rejections). \
         Saturation is the highest ladder step whose p99 overhead stayed within 5 ms with no errors. \
         Errors are the share of offered requests that failed; CPU is the target's processes' CPU \
         time over the window per completed request; RSS idle is read after start, RSS peak during \
         the window."
            .to_owned(),
    ));
    blocks.push(Block::Table(headers, rows));
    if !problems.is_empty() {
        blocks.push(Block::Heading(2, "Invalid or skipped".to_owned()));
        for p in &problems {
            blocks.push(Block::Para(p.clone()));
        }
    }
    for (name, text) in [
        ("comparison.md", to_markdown(&blocks)),
        ("comparison.html", to_html(&blocks)),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, text).map_err(|source| ReportError::Write { path, source })?;
    }
    write_json(
        &dir.join("comparison.json"),
        &json!({
            "schema_version": 1,
            "targets": runs.iter().map(|t| json!({"name": t.name, "version": t.run.target_version,
                "commit": t.run.target_commit, "not_publishable": t.run.not_publishable})).collect::<Vec<_>>(),
            "cells": json_cells,
        }),
    )?;
    Ok(Compared {
        targets: runs.len(),
        cells: order.len(),
    })
}

fn validity(s: &Summary) -> String {
    if let Some(why) = &s.skipped {
        format!("skipped: {why}")
    } else if s.valid {
        format!("yes ({})", s.repetitions)
    } else {
        format!("no ({})", s.repetitions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latency_a_cell_is_compared_by() {
        assert_eq!(family(&|k| k == "overhead_us.p50").1, "overhead_us");
        assert_eq!(family(&|k| k == "ttft_overhead_us.p50").1, "ttft_overhead_us");
        assert_eq!(family(&|k| k == "latency_us.p50").0, "latency");
        assert_eq!(family(&|_| false).0, "—");
    }
}

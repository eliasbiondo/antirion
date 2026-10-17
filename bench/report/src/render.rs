//! `report.md` and `report.html`: the run's facts, the gates, and one table per cell whose numbers
//! link to the cell's `summary.json`.

use std::fmt::Write as _;

use crate::gates::{Gate, Summaries};
use crate::model::{CellDef, RunManifest};
use crate::summary::{PERCENTILES, Summary};

/// A table cell: text, optionally a link.
#[derive(Debug, Clone)]
pub(crate) struct Cell {
    text: String,
    link: Option<String>,
}

pub(crate) fn plain(text: impl Into<String>) -> Cell {
    Cell {
        text: text.into(),
        link: None,
    }
}

pub(crate) fn linked(text: impl Into<String>, link: &str) -> Cell {
    Cell {
        text: text.into(),
        link: Some(link.to_owned()),
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Block {
    Heading(u8, String),
    Para(String),
    Table(Vec<String>, Vec<Vec<Cell>>),
}

pub(crate) fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        "—".to_owned()
    } else if v.abs() >= 100.0 || v.fract().abs() < 1e-9 {
        format!("{v:.0}")
    } else {
        format!("{v:.2}")
    }
}

pub(crate) fn level_text(def: &CellDef) -> String {
    let l = &def.level;
    if let Some(n) = l.concurrent_streams {
        format!("{n} concurrent streams")
    } else if let Some(n) = l.concurrency {
        format!("closed loop, {n} connections")
    } else if l.ladder.is_some() {
        "saturation ladder".to_owned()
    } else if let Some(r) = l.rps {
        match l.total_requests {
            Some(n) => format!("{} RPS, {n} requests", fmt_num(r)),
            None => format!("{} RPS", fmt_num(r)),
        }
    } else {
        "no load".to_owned()
    }
}

fn stat_cell(s: &Summary, key: &str, link: &str, noise: Option<f64>) -> Cell {
    match s.stats.get(key) {
        None => plain("—"),
        Some((med, lo, hi)) => {
            let mut text = format!("{} [{}–{}]", fmt_num(*med), fmt_num(*lo), fmt_num(*hi));
            if noise.is_some_and(|n| med.abs() < n) {
                text = format!("within noise ({text})");
            }
            linked(text, link)
        }
    }
}

fn cell_blocks(def: &CellDef, s: &Summary) -> Vec<Block> {
    let link = format!("cells/{}/summary.json", def.cell);
    let mut title = format!("{} — {}", def.cell, def.kind_name());
    for part in [&def.scenario, &def.profile, &def.mock_profile]
        .into_iter()
        .flatten()
    {
        let _ = write!(title, ", {part}");
    }
    let _ = write!(title, ", {}", level_text(def));
    if def.protocol != "h1" {
        let _ = write!(title, ", {}", def.protocol);
    }
    let mut out = vec![Block::Heading(3, title)];
    let mut status = if s.valid {
        format!("Valid, {} repetitions.", s.repetitions)
    } else {
        format!(
            "**Invalid** ({} repetitions): {}",
            s.repetitions,
            s.reasons.join("; ")
        )
    };
    if def.informational {
        status.push_str(" Informational: no pass condition.");
    }
    let _ = write!(status, " [summary.json]({link})");
    out.push(Block::Para(status));
    let mut headers = vec!["µs".to_owned()];
    headers.extend(PERCENTILES.iter().map(|(p, _)| p.replace('_', ".")));
    headers.extend(["max".to_owned(), "mean".to_owned()]);
    let pcts: Vec<&str> = PERCENTILES
        .iter()
        .map(|(p, _)| *p)
        .chain(["max", "mean"])
        .collect();
    let mut rows = Vec::new();
    for (label, prefix, noisy) in [
        ("Overhead", "overhead_us", true),
        ("TTFT overhead", "ttft_overhead_us", true),
        ("Inter-event overhead", "inter_event_overhead_us", true),
        ("Gateway latency", "latency_us", false),
        ("Direct latency", "direct_latency_us", false),
        ("x-antirion-overhead-ms", "overhead_header_us", false),
    ] {
        if !s.stats.keys().any(|k| k.starts_with(&format!("{prefix}."))) {
            continue;
        }
        let mut row = vec![plain(label)];
        for p in &pcts {
            let noise = if noisy { s.direct_spread(p) } else { None };
            row.push(stat_cell(s, &format!("{prefix}.{p}"), &link, noise));
        }
        rows.push(row);
    }
    if !rows.is_empty() {
        out.push(Block::Table(headers, rows));
    }
    let shown = |k: &str| {
        [
            "overhead_us.",
            "ttft_overhead_us.",
            "inter_event_overhead_us.",
            "latency_us.",
            "direct_latency_us.",
            "overhead_header_us.",
            "ttft_us.",
            "direct.",
        ]
        .iter()
        .any(|p| k.starts_with(p))
    };
    let rows: Vec<Vec<Cell>> = s
        .stats
        .keys()
        .filter(|k| !shown(k))
        .map(|k| vec![plain(k.clone()), stat_cell(s, k, &link, None)])
        .collect();
    if !rows.is_empty() {
        out.push(Block::Table(
            vec!["Metric".to_owned(), "median [min–max]".to_owned()],
            rows,
        ));
    }
    out
}

impl CellDef {
    fn kind_name(&self) -> &'static str {
        use crate::model::Kind;
        match self.kind {
            Kind::Open => "open loop",
            Kind::Closed => "closed loop",
            Kind::Saturation => "saturation",
            Kind::Hold => "held streams",
            Kind::Idle => "idle",
            Kind::Startup => "startup",
            Kind::Apply => "configuration apply",
            Kind::Micro => "micro-benchmark",
        }
    }
}

fn blocks(run: &RunManifest, all: &Summaries, gates: &[Gate]) -> Vec<Block> {
    let mut out = vec![Block::Heading(1, "Benchmark report".to_owned())];
    if !run.reference {
        out.push(Block::Para(format!(
            "**Topology `{}` is not a reference topology.** Gates are shown with the verdict they \
             would have, but no result of this run claims one.",
            run.topology
        )));
    }
    if !run.not_publishable.is_empty() {
        out.push(Block::Para(format!(
            "**Not publishable:** {}.",
            run.not_publishable.join("; ")
        )));
    }
    out.push(Block::Table(
        vec!["Run".to_owned(), String::new()],
        vec![
            vec![plain("Suite"), plain(run.suite.clone())],
            vec![plain("Topology"), plain(run.topology.clone())],
            vec![
                plain("Target"),
                plain(format!("{} {}", run.target, run.target_version)),
            ],
            vec![plain("Target commit"), plain(run.target_commit.clone())],
            vec![plain("Harness commit"), plain(run.harness_commit.clone())],
            vec![plain("Corpus seed"), plain(run.corpus_seed.to_string())],
            vec![plain("Started"), plain(run.started_at.clone())],
            vec![plain("Ended"), plain(run.ended_at.clone())],
            vec![plain("Manifest"), linked("run.json", "run.json")],
        ],
    ));
    if !gates.is_empty() {
        out.push(Block::Heading(2, "Gates".to_owned()));
        let rows = gates
            .iter()
            .map(|g| {
                let link = format!(
                    "cells/{}/summary.json",
                    g.cell.split('/').next().unwrap_or(&g.cell)
                );
                let mut status = g.status.clone();
                if let Some(i) = &g.indicative {
                    let _ = write!(status, " (would {i})");
                }
                vec![
                    linked(g.cell.clone(), &link),
                    plain(g.metric.clone()),
                    plain(format!(
                        "{} {}",
                        g.comparison,
                        g.threshold.map_or("—".to_owned(), fmt_num)
                    )),
                    g.value.map_or(plain("—"), |v| linked(fmt_num(v), &link)),
                    plain(status),
                    plain(g.note.clone().unwrap_or_default()),
                ]
            })
            .collect();
        out.push(Block::Table(
            ["Cell", "Metric", "Condition", "Value", "Status", "Note"]
                .map(str::to_owned)
                .to_vec(),
            rows,
        ));
    }
    out.push(Block::Heading(2, "Cells".to_owned()));
    out.push(Block::Para(
        "Each number is the median over the repetitions with [min–max]. An overhead smaller than the \
         spread of the direct run at the same percentile is shown as within noise."
            .to_owned(),
    ));
    for (def, s) in all.values() {
        out.extend(cell_blocks(def, s));
    }
    out
}

fn md_escape(s: &str) -> String {
    s.replace('|', "\\|")
}

pub(crate) fn to_markdown(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        match b {
            Block::Heading(n, t) => {
                let _ = writeln!(out, "{} {t}\n", "#".repeat(usize::from(*n)));
            }
            Block::Para(t) => {
                let _ = writeln!(out, "{t}\n");
            }
            Block::Table(h, rows) => {
                let _ = writeln!(
                    out,
                    "| {} |",
                    h.iter().map(|x| md_escape(x)).collect::<Vec<_>>().join(" | ")
                );
                let _ = writeln!(out, "|{}", "---|".repeat(h.len()));
                for r in rows {
                    let cells: Vec<String> = r
                        .iter()
                        .map(|c| match &c.link {
                            Some(l) => format!("[{}]({l})", md_escape(&c.text)),
                            None => md_escape(&c.text),
                        })
                        .collect();
                    let _ = writeln!(out, "| {} |", cells.join(" | "));
                }
                out.push('\n');
            }
        }
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The paragraph's own light markup: `**bold**`, `` `code` `` and `[text](link)`.
fn inline_html(s: &str) -> String {
    let mut out = String::new();
    let mut rest = html_escape(s);
    while let Some(i) = rest.find("[") {
        let Some(j) = rest[i..].find("](").map(|j| i + j) else {
            break;
        };
        let Some(k) = rest[j..].find(')').map(|k| j + k) else {
            break;
        };
        out.push_str(&rest[..i]);
        let _ = write!(out, "<a href=\"{}\">{}</a>", &rest[j + 2..k], &rest[i + 1..j]);
        rest = rest[k + 1..].to_owned();
    }
    out.push_str(&rest);
    let mut bold = false;
    let parts: Vec<&str> = out.split("**").collect();
    let mut html = String::new();
    for (n, p) in parts.iter().enumerate() {
        if n > 0 {
            html.push_str(if bold { "</strong>" } else { "<strong>" });
            bold = !bold;
        }
        html.push_str(p);
    }
    let mut code = false;
    let parts: Vec<String> = html.split('`').map(str::to_owned).collect();
    let mut done = String::new();
    for (n, p) in parts.iter().enumerate() {
        if n > 0 {
            done.push_str(if code { "</code>" } else { "<code>" });
            code = !code;
        }
        done.push_str(p);
    }
    done
}

pub(crate) fn to_html(blocks: &[Block]) -> String {
    let mut out = String::from(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Benchmark report</title><style>\
         :root{--fg:#1d1d1f;--bg:#fff;--muted:#6e6e73;--line:#d2d2d7;--link:#0a66c2}\
         @media (prefers-color-scheme: dark){:root{--fg:#f5f5f7;--bg:#111;--muted:#a1a1a6;--line:#3a3a3c;--link:#6cb4ff}}\
         body{font:14px/1.5 system-ui,sans-serif;color:var(--fg);background:var(--bg);margin:0 auto;\
         max-width:1200px;padding:16px}table{border-collapse:collapse;margin:8px 0 20px;display:block;\
         overflow-x:auto}th,td{border-bottom:1px solid var(--line);padding:4px 10px;text-align:left;\
         white-space:nowrap}th{color:var(--muted);font-weight:600}a{color:var(--link)}\
         </style></head><body>\n",
    );
    for b in blocks {
        match b {
            Block::Heading(n, t) => {
                let _ = writeln!(out, "<h{n}>{}</h{n}>", html_escape(t));
            }
            Block::Para(t) => {
                let _ = writeln!(out, "<p>{}</p>", inline_html(t));
            }
            Block::Table(h, rows) => {
                out.push_str("<table><thead><tr>");
                for x in h {
                    let _ = write!(out, "<th>{}</th>", html_escape(x));
                }
                out.push_str("</tr></thead><tbody>\n");
                for r in rows {
                    out.push_str("<tr>");
                    for c in r {
                        match &c.link {
                            Some(l) => {
                                let _ = write!(
                                    out,
                                    "<td><a href=\"{}\">{}</a></td>",
                                    html_escape(l),
                                    html_escape(&c.text)
                                );
                            }
                            None => {
                                let _ = write!(out, "<td>{}</td>", html_escape(&c.text));
                            }
                        }
                    }
                    out.push_str("</tr>\n");
                }
                out.push_str("</tbody></table>\n");
            }
        }
    }
    out.push_str("</body></html>\n");
    out
}

/// The report as Markdown and as HTML.
#[must_use]
pub fn render(run: &RunManifest, all: &Summaries, gates: &[Gate]) -> (String, String) {
    let b = blocks(run, all, gates);
    (to_markdown(&b), to_html(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_markup() {
        assert_eq!(
            inline_html("**Bold** `x` [s](a/b.json) <"),
            "<strong>Bold</strong> <code>x</code> <a href=\"a/b.json\">s</a> &lt;"
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(fmt_num(1234.567), "1235");
        assert_eq!(fmt_num(12.345), "12.35");
        assert_eq!(fmt_num(3.0), "3");
    }
}

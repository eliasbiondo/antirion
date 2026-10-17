//! The report generator: summaries of every cell from the raw results, the pass conditions, the
//! regression gate against a baseline, and the rendered report.

#![forbid(unsafe_code)]

pub mod compare;
pub mod gates;
pub mod hdr;
pub mod model;
pub mod render;
pub mod summary;

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::gates::{Gate, Summaries};
use crate::model::{CellDef, RunManifest, read_json, write_json};

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("cannot read {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("cannot write {path}: {source}")]
    Write { path: PathBuf, source: std::io::Error },
    #[error("{path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("histogram log: {0}")]
    Hdr(String),
}

/// Reads and summarises every cell of a result directory.
pub fn summaries(dir: &Path) -> Result<Summaries, ReportError> {
    let cells = dir.join("cells");
    let mut out = Summaries::new();
    let Ok(entries) = std::fs::read_dir(&cells) else {
        return Ok(out);
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for d in dirs {
        let def_path = d.join("cell.json");
        if !def_path.exists() {
            continue;
        }
        let def: CellDef = read_json(&def_path)?;
        let s = summary::cell(&d)?;
        out.insert(def.cell.clone(), (def, s));
    }
    Ok(out)
}

/// What [`render`] decided.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub gates: Vec<Gate>,
    pub cells: usize,
    pub invalid_cells: usize,
}

impl Outcome {
    #[must_use]
    pub fn count(&self, status: &str) -> usize {
        self.gates.iter().filter(|g| g.status == status).count()
    }
}

/// Writes every cell's `summary.json`, then `gates.json`, `report.md` and `report.html` into `dir`.
/// Cells of the `with` runs are used by gates that compare across runs (a scaling factor between
/// topologies), never reported as this run's.
pub fn render(dir: &Path, baseline: Option<&Path>, with: &[PathBuf]) -> Result<Outcome, ReportError> {
    let run: RunManifest = read_json(&dir.join("run.json"))?;
    let all = summaries(dir)?;
    for (id, (def, s)) in &all {
        write_json(
            &dir.join("cells").join(id).join("summary.json"),
            &summary::to_json(def, s),
        )?;
    }
    let mut lookup = all.clone();
    for other in with {
        for (id, entry) in summaries(other)? {
            lookup.entry(id).or_insert(entry);
        }
    }
    let mut gate_list = gates::evaluate_all(&all, &lookup);
    if let Some(base) = baseline {
        let before = summaries(base)?;
        gate_list.extend(gates::regressions(&all, &before, run.reference));
    }
    write_json(
        &dir.join("gates.json"),
        &json!({
            "schema_version": 1,
            "reference": run.reference,
            "baseline": baseline.map(|b| b.display().to_string()),
            "gates": gate_list,
        }),
    )?;
    let (md, html) = render::render(&run, &all, &gate_list);
    for (name, text) in [("report.md", md), ("report.html", html)] {
        let path = dir.join(name);
        std::fs::write(&path, text).map_err(|source| ReportError::Write { path, source })?;
    }
    Ok(Outcome {
        cells: all.len(),
        invalid_cells: all.values().filter(|(_, s)| !s.valid).count(),
        gates: gate_list,
    })
}

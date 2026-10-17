//! Pass conditions evaluated on the summaries, and the regression gate against a baseline run.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::model::{CellDef, GateDef, Threshold};
use crate::summary::Summary;

/// One entry of `gates.json`.
#[derive(Debug, Clone, Serialize)]
pub struct Gate {
    pub cell: String,
    pub metric: String,
    pub threshold: Option<f64>,
    pub comparison: String,
    pub value: Option<f64>,
    /// `pass`, `fail`, `invalid` or `n/a`.
    pub status: String,
    /// On a non-reference topology: what the status would be on a reference one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indicative: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Summaries of every cell of a run, by cell id.
pub type Summaries = BTreeMap<String, (CellDef, Summary)>;

/// `C12/saturation_rps` → (`C12`, `saturation_rps`); a bare metric is `own`'s.
fn split<'a>(own: &'a str, metric: &'a str) -> (&'a str, &'a str) {
    metric.split_once('/').unwrap_or((own, metric))
}

/// A cell's metric, and whether that cell is valid.
fn lookup(all: &Summaries, own: &str, metric: &str) -> Result<(f64, bool), String> {
    let (cell, path) = split(own, metric);
    let (_, s) = all.get(cell).ok_or_else(|| format!("cell {cell} was not run"))?;
    if let Some(why) = &s.skipped {
        return Err(format!("{cell} not run: {why}"));
    }
    s.median(path)
        .map(|v| (v, s.valid))
        .ok_or_else(|| format!("{cell} did not measure {path}"))
}

fn compare(value: f64, op: &str, threshold: f64) -> bool {
    match op {
        "<=" => value <= threshold,
        "<" => value < threshold,
        ">=" => value >= threshold,
        ">" => value > threshold,
        "==" => (value - threshold).abs() < f64::EPSILON,
        _ => false,
    }
}

fn evaluate(all: &Summaries, def: &CellDef, g: &GateDef) -> Gate {
    let mut gate = Gate {
        cell: def.cell.clone(),
        metric: match &g.minus {
            Some(m) => format!("{} - {m}", g.metric),
            None => g.metric.clone(),
        },
        threshold: None,
        comparison: g.op.clone(),
        value: None,
        status: "n/a".to_owned(),
        indicative: None,
        note: None,
    };
    if g.abs {
        gate.metric = format!("|{}|", gate.metric);
    }
    let value = lookup(all, &def.cell, &g.metric).and_then(|(v, ok)| match &g.minus {
        Some(m) => lookup(all, &def.cell, m).map(|(b, ok2)| (v - b, ok && ok2)),
        None => Ok((v, ok)),
    });
    let threshold = match &g.threshold {
        Threshold::Value(v) => Ok((*v, true)),
        Threshold::Relative { times, of } => lookup(all, &def.cell, of).map(|(b, ok)| (b * times, ok)),
    };
    match (value, threshold) {
        (Ok((v, valid_a)), Ok((t, valid_b))) => {
            let v = if g.abs { v.abs() } else { v };
            gate.value = Some(v);
            gate.threshold = Some(t);
            let verdict = if compare(v, &g.op, t) { "pass" } else { "fail" };
            if !(valid_a && valid_b) {
                "invalid".clone_into(&mut gate.status);
                gate.indicative = Some(verdict.to_owned());
                gate.note = Some("a cell it reads is invalid".to_owned());
            } else if def.reference {
                verdict.clone_into(&mut gate.status);
            } else {
                gate.indicative = Some(verdict.to_owned());
                gate.note = Some(format!("measured on {}, not a reference topology", def.topology));
            }
        }
        (Err(e), _) | (_, Err(e)) => gate.note = Some(e),
    }
    if let Threshold::Value(t) = g.threshold {
        gate.threshold = Some(t);
    }
    gate
}

/// Every gate of every cell of the run; `lookup` resolves the cells gates refer to (the run's own,
/// plus cells of other runs a cross-run gate needs).
#[must_use]
pub fn evaluate_all(current: &Summaries, lookup: &Summaries) -> Vec<Gate> {
    let mut out = Vec::new();
    for (def, s) in current.values() {
        if def.informational {
            continue;
        }
        for g in &def.gates {
            let mut gate = evaluate(lookup, def, g);
            if !s.valid && s.skipped.is_none() && gate.status != "n/a" {
                "invalid".clone_into(&mut gate.status);
            }
            out.push(gate);
        }
    }
    out
}

/// The regression gate: p99 overhead or saturation throughput more than 10 % worse than the baseline,
/// or idle RSS more than 10 % higher, fails.
#[must_use]
pub fn regressions(current: &Summaries, baseline: &Summaries, reference: bool) -> Vec<Gate> {
    const CHECKS: [(&str, bool); 3] = [
        ("overhead_us.p99", true),
        ("saturation_rps", false),
        ("rss_bytes.idle", true),
    ];
    let mut out = Vec::new();
    for (cell, (def, s)) in current {
        let Some((_, base)) = baseline.get(cell) else {
            continue;
        };
        for (metric, higher_is_worse) in CHECKS {
            let (Some(now), Some(then)) = (s.median(metric), base.median(metric)) else {
                continue;
            };
            let (op, limit) = if higher_is_worse {
                ("<=", then + then.abs() * 0.10)
            } else {
                (">=", then - then.abs() * 0.10)
            };
            let verdict = if compare(now, op, limit) { "pass" } else { "fail" };
            let mut gate = Gate {
                cell: cell.clone(),
                metric: format!("{metric} vs baseline"),
                threshold: Some(limit),
                comparison: op.to_owned(),
                value: Some(now),
                status: verdict.to_owned(),
                indicative: None,
                note: Some(format!("baseline {then:.1}")),
            };
            if !(s.valid && base.valid) {
                "invalid".clone_into(&mut gate.status);
            } else if !(reference && def.reference) {
                "n/a".clone_into(&mut gate.status);
                gate.indicative = Some(verdict.to_owned());
            }
            out.push(gate);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kind;

    fn def(cell: &str, reference: bool, gates: Vec<GateDef>) -> CellDef {
        serde_json::from_value::<CellDef>(serde_json::json!({"cell": cell, "kind": "open"}))
            .map(|mut d| {
                d.reference = reference;
                d.gates = gates;
                d.kind = Kind::Open;
                d
            })
            .unwrap()
    }

    fn summary(pairs: &[(&str, f64)]) -> Summary {
        Summary {
            stats: pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v, *v, *v)))
                .collect(),
            valid: true,
            reasons: vec![],
            repetitions: 5,
            skipped: None,
        }
    }

    fn gate(metric: &str, op: &str, t: Threshold) -> GateDef {
        GateDef {
            metric: metric.to_owned(),
            op: op.to_owned(),
            threshold: t,
            minus: None,
            abs: false,
        }
    }

    #[test]
    fn reference_gates_pass_and_fail_others_are_indicative() {
        let mut all = Summaries::new();
        all.insert(
            "C1".into(),
            (
                def(
                    "C1",
                    true,
                    vec![gate("overhead_us.p99", "<=", Threshold::Value(500.0))],
                ),
                summary(&[("overhead_us.p99", 420.0)]),
            ),
        );
        all.insert(
            "C2".into(),
            (
                def(
                    "C2",
                    false,
                    vec![gate("overhead_us.p99", "<=", Threshold::Value(500.0))],
                ),
                summary(&[("overhead_us.p99", 900.0)]),
            ),
        );
        let gates = evaluate_all(&all, &all);
        assert_eq!(gates[0].status, "pass");
        assert_eq!(gates[1].status, "n/a");
        assert_eq!(gates[1].indicative.as_deref(), Some("fail"));
    }

    #[test]
    fn relative_and_difference_gates() {
        let mut all = Summaries::new();
        all.insert(
            "C12".into(),
            (def("C12", true, vec![]), summary(&[("saturation_rps", 15000.0)])),
        );
        let g = gate(
            "saturation_rps",
            ">=",
            Threshold::Relative {
                times: 3.2,
                of: "C12/saturation_rps".into(),
            },
        );
        all.insert(
            "C15".into(),
            (def("C15", true, vec![g]), summary(&[("saturation_rps", 47000.0)])),
        );
        let mut d = gate("C27.a/overhead_us.p99", "<=", Threshold::Value(20.0));
        d.minus = Some("C27.b/overhead_us.p99".into());
        all.insert(
            "C27.a".into(),
            (
                def("C27.a", true, vec![d]),
                summary(&[("overhead_us.p99", 530.0)]),
            ),
        );
        all.insert(
            "C27.b".into(),
            (def("C27.b", true, vec![]), summary(&[("overhead_us.p99", 500.0)])),
        );
        let gates = evaluate_all(&all, &all);
        let c15 = gates.iter().find(|g| g.cell == "C15").unwrap();
        assert_eq!(c15.status, "fail");
        assert_eq!(c15.threshold, Some(48000.0));
        let c27 = gates.iter().find(|g| g.cell == "C27.a").unwrap();
        assert_eq!(c27.value, Some(30.0));
        assert_eq!(c27.status, "fail");
    }

    #[test]
    fn invalid_cells_fail_their_gates_as_invalid() {
        let mut all = Summaries::new();
        let mut s = summary(&[("overhead_us.p99", 1.0)]);
        s.valid = false;
        all.insert(
            "C1".into(),
            (
                def(
                    "C1",
                    true,
                    vec![gate("overhead_us.p99", "<=", Threshold::Value(5.0))],
                ),
                s,
            ),
        );
        assert_eq!(evaluate_all(&all, &all)[0].status, "invalid");
    }

    #[test]
    fn regression_gate() {
        let mut now = Summaries::new();
        let mut base = Summaries::new();
        now.insert(
            "N1".into(),
            (def("N1", true, vec![]), summary(&[("overhead_us.p99", 560.0)])),
        );
        base.insert(
            "N1".into(),
            (def("N1", true, vec![]), summary(&[("overhead_us.p99", 500.0)])),
        );
        let g = regressions(&now, &base, true);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].status, "fail");
        now.insert(
            "N1".into(),
            (def("N1", true, vec![]), summary(&[("overhead_us.p99", 540.0)])),
        );
        assert_eq!(regressions(&now, &base, true)[0].status, "pass");
    }
}

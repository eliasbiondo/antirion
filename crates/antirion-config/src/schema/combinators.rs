//! Combinators that need a choice among failing branches: which branch explains an `anyOf` or
//! `oneOf` failure best, and the special forms whose violations get their own detail code.

use super::compile::Schema;
use super::validate::{Outcome, push};
use crate::{DetailCode, DocPath};

/// When every branch is `{required: [x]}`, their field names.
pub(super) fn required_only(branches: &[Schema]) -> Option<Vec<String>> {
    let mut names = Vec::new();
    for b in branches {
        match b {
            Schema::Node(n) if n.required.len() == 1 && n.reference.is_none() && n.types.is_none() => {
                names.push(n.required.first()?.clone());
            }
            _ => return None,
        }
    }
    Some(names)
}

pub(super) fn is_reserved_names(s: &Schema) -> bool {
    match s {
        Schema::Node(n) => n.enumeration.as_ref().is_some_and(|e| {
            e.iter()
                .all(|v| matches!(v.as_str(), Some("locked" | "request_overridable")))
        }),
        Schema::Bool(_) => false,
    }
}

/// Chooses the failing branch that explains the error best: branches rejected only for their type
/// are ignored unless all are, then the deepest error wins, then the fewest errors, then the first.
pub(super) fn best_branch(results: Vec<Outcome>, path: &DocPath) -> Outcome {
    let type_only = |o: &Outcome| {
        o.violations.len() == 1
            && o.violations
                .iter()
                .all(|v| v.code == DetailCode::TypeMismatch && v.path == *path)
    };
    if results.iter().all(type_only) {
        let mut expected: Vec<String> = Vec::new();
        for r in &results {
            for v in &r.violations {
                if let Some(rest) = v.message.strip_prefix("expected ")
                    && let Some((what, _)) = rest.split_once(", got ")
                    && !expected.iter().any(|e| e == what)
                {
                    expected.push(what.to_owned());
                }
            }
        }
        let got = results
            .first()
            .and_then(|r| r.violations.first())
            .and_then(|v| v.message.split_once(", got ").map(|(_, g)| g.to_owned()))
            .unwrap_or_default();
        let mut out = Outcome::default();
        push(
            &mut out,
            path,
            DetailCode::TypeMismatch,
            format!("expected {}, got {got}", expected.join(" or ")),
        );
        return out;
    }
    let enum_only = |o: &Outcome| {
        o.violations
            .iter()
            .all(|v| v.code == DetailCode::InvalidEnum && v.path == *path)
    };
    let depth = |o: &Outcome| o.violations.iter().map(|v| v.path.len()).max().unwrap_or(0);
    let mut candidates: Vec<(usize, Outcome)> = results
        .into_iter()
        .enumerate()
        .filter(|(_, r)| !type_only(r))
        .collect();
    candidates.sort_by(|(ia, a), (ib, b)| {
        depth(b)
            .cmp(&depth(a))
            .then_with(|| enum_only(a).cmp(&enum_only(b)))
            .then_with(|| a.violations.len().cmp(&b.violations.len()))
            .then_with(|| ia.cmp(ib))
    });
    candidates.into_iter().next().map(|(_, o)| o).unwrap_or_default()
}

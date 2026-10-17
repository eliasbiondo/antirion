//! The request and response transforms of a stage: the request's template (input, before every other transform) and
//! JSON repair of the response's content (output).

use std::sync::Arc;

use serde_json::Value;

use crate::def::{Check, Def, SegKind};
use crate::segments::{Dialect, output_segments, write_back};
use crate::template::{TemplateError, Templates};

/// Applies the request's template with the stage's template guardrails; `None` when none applies
/// to the dialect, else whether the body changed.
pub(super) fn templates(
    defs: &[Arc<Def>],
    body: &mut Value,
    dialect: Dialect,
) -> Result<Option<bool>, TemplateError> {
    let templates: Vec<&Templates> = defs
        .iter()
        .filter_map(|d| match &d.check {
            Check::Template(t) => Some(t),
            _ => None,
        })
        .collect();
    if templates.is_empty() || crate::template::target(dialect).is_none() {
        return Ok(None);
    }
    crate::template::apply(body, dialect, &templates).map(Some)
}

/// Repairs the JSON of every content segment of a response; whether any changed.
pub(super) fn repair_content(body: &mut Value, dialect: Dialect) -> bool {
    let mut changed = false;
    for mut seg in output_segments(body, dialect) {
        if seg.kind != SegKind::Content {
            continue;
        }
        if let Some(fixed) = crate::repair::repair(&seg.text) {
            seg.text = fixed;
            write_back(body, &seg);
            changed = true;
        }
    }
    changed
}

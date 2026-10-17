//! Request-selected guardrails: `x-antirion-guardrail-select` adds guardrails the effective policy
//! lists in `request_selectable`, at the stages each one names in `request_stages`. A request can
//! only add guardrails, never remove or configure one.

use std::sync::Arc;

use antirion_core::Stage;
use antirion_error::{GatewayError, Message};
use antirion_guardrails::{Def, GuardStage};
use http::HeaderMap;
use serde_json::Value;

use crate::guard::RequestGuard;

/// The request header that selects guardrails.
pub(crate) const HEADER: &str = "x-antirion-guardrail-select";

/// Reasons of request-selected guardrails.
pub(crate) mod reasons {
    antirion_error::reasons! {
        UNKNOWN_GUARDRAIL = "unknown_guardrail" => InvalidRequest,
        GUARDRAIL_NOT_SELECTABLE = "guardrail_not_selectable" => Forbidden,
    }
}

/// The names the header lists, in order (several header lines read as one list).
fn names(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(HEADER)
        .iter()
        .flat_map(|v| v.to_str().unwrap_or("\u{fffd}").split(','))
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Adds the guardrails the request selects to `g`; `merged` is the effective `policies.guardrails`.
pub(crate) fn apply(
    headers: &HeaderMap,
    g: &mut RequestGuard,
    defs: &antirion_guardrails::Guardrails,
    merged: &Value,
) -> Result<(), GatewayError> {
    let wanted = names(headers);
    if wanted.is_empty() {
        return Ok(());
    }
    let selectable: Vec<&str> = merged
        .get("request_selectable")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut chosen: Vec<Arc<Def>> = Vec::new();
    for name in &wanted {
        let Some(def) = defs.get(name) else {
            return Err(GatewayError::with_reason(
                reasons::UNKNOWN_GUARDRAIL,
                Stage::GuardInput,
                Message::built(format!("The guardrail `{name}` does not exist.")),
            )
            .param(HEADER));
        };
        if !selectable.contains(&name.as_str()) {
            return Err(GatewayError::with_reason(
                reasons::GUARDRAIL_NOT_SELECTABLE,
                Stage::GuardInput,
                Message::built(format!("The guardrail `{name}` cannot be selected by a request.")),
            )
            .param(HEADER));
        }
        if !chosen.iter().any(|d| d.name == def.name) {
            chosen.push(Arc::clone(def));
        }
    }
    chosen.sort_by(|a, b| a.order.cmp(&b.order).then(a.name.cmp(&b.name)));
    for def in chosen {
        for stage in &def.request_stages {
            let list = match stage {
                GuardStage::Input => &mut g.attached.input,
                GuardStage::Output => &mut g.attached.output,
            };
            if !list.iter().any(|d| d.name == def.name) {
                list.push(Arc::clone(&def));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use super::*;

    #[test]
    fn names_are_a_comma_separated_list() {
        let mut h = HeaderMap::new();
        h.append(HEADER, HeaderValue::from_static(" a, b ,,"));
        h.append(HEADER, HeaderValue::from_static("c"));
        assert_eq!(names(&h), ["a", "b", "c"]);
    }
}

//! `pricing.report_in_body`: a complete OpenAI- or Anthropic-dialect JSON response gets its settled
//! cost as `usage.cost` and `usage.cost_details` (the cost groups), as JSON numbers in USD.

use antirion_dialects::{Dialect, Endpoint};
use antirion_limits::cost::CostComponents;
use antirion_limits::money::Nusd;
use http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use http::{HeaderValue, Response};
use http_body_util::BodyExt;
use serde_json::{Map, Value};

use super::Slot;
use crate::ctx::{Body, full};

/// A USD amount as a JSON number. Amounts carry at most 9 decimals, which a double renders back
/// exactly for any realistic cost.
pub(crate) fn number(n: Nusd) -> Value {
    n.to_usd_string()
        .parse::<f64>()
        .ok()
        .and_then(|f| serde_json::Number::from_f64(f).map(Value::Number))
        .unwrap_or(Value::Null)
}

pub(crate) fn details(c: &CostComponents) -> Value {
    Value::Object(
        c.named()
            .iter()
            .filter(|(_, n)| n.0 != 0)
            .map(|(k, n)| ((*k).to_owned(), number(*n)))
            .collect::<Map<String, Value>>(),
    )
}

fn reports(endpoint: &Endpoint) -> bool {
    matches!(
        endpoint.dialect,
        Dialect::OpenAiChat
            | Dialect::OpenAiCompletions
            | Dialect::OpenAiResponses
            | Dialect::OpenAiEmbeddings
            | Dialect::AnthropicMessages
    )
}

/// Adds the cost to the body's `usage` object; the body unchanged when it has none.
fn with_cost(body: &[u8], cost: Nusd, components: &CostComponents) -> Option<Vec<u8>> {
    let mut v: Value = serde_json::from_slice(body).ok()?;
    let usage = v.get_mut("usage")?.as_object_mut()?;
    usage.insert("cost".into(), number(cost));
    usage.insert("cost_details".into(), details(components));
    serde_json::to_vec(&v).ok()
}

/// The response with its cost in the body, when the setting asks for it and the response is a
/// complete JSON answer with a `usage` object.
pub(crate) async fn report(
    enabled: bool,
    endpoint: &Endpoint,
    slot: &Slot,
    resp: Response<Body>,
) -> Response<Body> {
    let json = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !enabled || !reports(endpoint) || !resp.status().is_success() || !json {
        return resp;
    }
    let Some((cost, components)) = slot.cost_breakdown() else {
        return resp;
    };
    let (mut parts, body) = resp.into_parts();
    let Ok(collected) = body.collect().await else {
        return Response::from_parts(parts, crate::ctx::empty());
    };
    let bytes = collected.to_bytes();
    let out = match with_cost(&bytes, cost, &components) {
        Some(edited) => {
            parts
                .headers
                .insert(CONTENT_LENGTH, HeaderValue::from(edited.len()));
            bytes::Bytes::from(edited)
        }
        None => bytes,
    };
    Response::from_parts(parts, full(out))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_cost_goes_into_usage() {
        let c = CostComponents {
            output: Nusd(1_500_000),
            cache_read: Nusd(500_000),
            ..CostComponents::default()
        };
        let body = serde_json::to_vec(&json!({"id": "x", "usage": {"prompt_tokens": 1}})).unwrap();
        let out: Value = serde_json::from_slice(&with_cost(&body, Nusd(2_000_000), &c).unwrap()).unwrap();
        assert_eq!(out["usage"]["cost"], json!(0.002));
        assert_eq!(
            out["usage"]["cost_details"],
            json!({"cache_read": 0.0005, "output": 0.0015})
        );
        assert!(with_cost(br#"{"data": []}"#, Nusd(1), &c).is_none());
    }
}

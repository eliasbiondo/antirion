//! `pricing.report_in_body` on streams: the events that carry usage are held until the stream is
//! settled, then sent with `usage.cost` and `usage.cost_details` added to that usage.

use antirion_limits::cost::CostComponents;
use antirion_limits::money::Nusd;
use bytes::Bytes;
use serde_json::Value;

/// Whether an event's JSON reports usage: a Chat usage chunk, an Anthropic `message_delta` or a
/// Responses `response.completed`.
pub(super) fn carries_usage(json: &Value) -> bool {
    json.get("usage").is_some_and(Value::is_object)
        || json.pointer("/response/usage").is_some_and(Value::is_object)
}

fn usage_of(v: &mut Value) -> Option<&mut serde_json::Map<String, Value>> {
    if v.get("usage").is_some_and(Value::is_object) {
        return v.get_mut("usage")?.as_object_mut();
    }
    v.pointer_mut("/response/usage")?.as_object_mut()
}

/// The frame with the cost added to the usage its `data:` line reports; unchanged when it reports
/// none.
pub(super) fn with_cost(frame: &Bytes, cost: Nusd, components: &CostComponents) -> Bytes {
    let Ok(text) = std::str::from_utf8(frame) else {
        return frame.clone();
    };
    let mut changed = false;
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        let ending = line.get(content.len()..).unwrap_or_default();
        let parsed = content
            .strip_prefix("data:")
            .map(str::trim_start)
            .and_then(|d| serde_json::from_str::<Value>(d).ok());
        let Some(mut v) = parsed else {
            out.push_str(line);
            continue;
        };
        let Some(usage) = usage_of(&mut v) else {
            out.push_str(line);
            continue;
        };
        usage.insert("cost".into(), crate::account::cost_number(cost));
        usage.insert(
            "cost_details".into(),
            crate::account::cost_details_json(components),
        );
        out.push_str("data: ");
        out.push_str(&v.to_string());
        out.push_str(ending);
        changed = true;
    }
    if changed { Bytes::from(out) } else { frame.clone() }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn usage_frames_get_the_cost() {
        let c = CostComponents {
            output: Nusd(2_000_000),
            ..CostComponents::default()
        };
        let chat = Bytes::from("data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1}}\n\n");
        let out = with_cost(&chat, Nusd(2_000_000), &c);
        let text = std::str::from_utf8(&out).unwrap();
        let v: Value = serde_json::from_str(text.trim().strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(v["usage"]["cost"], json!(0.002));
        assert!(text.ends_with("\n\n"));
        let anthropic = Bytes::from(
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3}}\n\n",
        );
        let out = with_cost(&anthropic, Nusd(1), &c);
        assert!(
            std::str::from_utf8(&out)
                .unwrap()
                .starts_with("event: message_delta\ndata: {")
        );
        let plain = Bytes::from("data: {\"choices\":[]}\n\n");
        assert_eq!(with_cost(&plain, Nusd(1), &c), plain);
        assert!(carries_usage(&json!({"response": {"usage": {}}})));
        assert!(!carries_usage(&json!({"usage": null})));
    }
}

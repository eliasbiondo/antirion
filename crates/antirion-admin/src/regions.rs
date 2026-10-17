//! A deployment's region in its read-only metadata: the value the region allow-lists match.

use serde_json::Value;

use crate::call::Call;

pub(crate) fn decorate(call: &Call, name: &str, body: &mut Value) {
    let region = call
        .snap
        .providers
        .deployments
        .get(name)
        .and_then(|d| d.effective_region())
        .map(str::to_owned);
    if let Some(obj) = body.get_mut("metadata").and_then(Value::as_object_mut) {
        obj.insert("region".into(), Value::from(region));
    }
}

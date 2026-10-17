//! What a response reveals about its upstream: `policies.headers.expose_upstream_identity: false`, at
//! the narrowest level that sets it, removes the routing-identity headers (the record keeps them).

use antirion_core::headers as h;
use antirion_runtime::identity::KeyRt;
use http::Response;
use serde_json::Value;

use crate::ctx::{Body, Ctx};
use crate::policy::Levels;

const IDENTITY: [&str; 5] = [
    h::PROVIDER,
    h::MODEL,
    h::DEPLOYMENT,
    h::UPSTREAM_STATUS,
    h::UPSTREAM_REQUEST_ID,
];

/// Whether the merged `headers` policy keeps the upstream identity visible.
fn exposed(effective: &Value) -> bool {
    effective
        .get("expose_upstream_identity")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

/// Removes the routing-identity headers when the request's levels hide them.
pub(crate) fn conceal(cx: &Ctx, key: &KeyRt, resp: &mut Response<Body>) {
    let (model, route) = cx
        .log
        .read(|r| (r.model.clone(), r.route.clone()))
        .unwrap_or_default();
    let levels = Levels::new(
        &cx.snap,
        key,
        model.as_deref().unwrap_or_default(),
        route.as_deref(),
    );
    if exposed(&levels.effective("headers")) {
        return;
    }
    cx.log.update(|r| r.conceal_identity = true);
    let headers = resp.headers_mut();
    for name in IDENTITY {
        headers.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn identity_is_exposed_unless_turned_off() {
        assert!(exposed(&json!({})));
        assert!(exposed(&json!({"expose_upstream_identity": true})));
        assert!(!exposed(&json!({"expose_upstream_identity": false})));
    }
}

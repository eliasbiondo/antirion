//! What the fuzz targets share: a request body read the way the data plane reads one.

#![forbid(unsafe_code)]

use antirion_dialects::endpoints::{Matched, match_endpoint};
use antirion_dialects::validate::{ValidateOpts, validate_json};
use antirion_dialects::xlate::decode::decode;
use antirion_dialects::xlate::ir::Client;
use antirion_json::{JsonLimits, skim};
use http::Method;

/// A request body sent to `path`: the strict JSON check, the endpoint's validation table, then the
/// dialect's decoder.
pub fn request(client: Client, path: &str, data: &[u8]) {
    let Ok(skimmed) = skim(data, &JsonLimits::default()) else {
        return;
    };
    let doc: Option<serde_json::Value> = serde_json::from_slice(data).ok();
    if let Matched::Found(endpoint) = match_endpoint(&Method::POST, path) {
        let _ = validate_json(&endpoint, data, &skimmed, doc.as_ref(), &ValidateOpts::default());
    }
    if let Some(v) = doc {
        let _ = decode(client, &v);
    }
}

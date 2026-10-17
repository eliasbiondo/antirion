#![no_main]

use std::net::{IpAddr, Ipv4Addr};

use antirion_runtime::security::SecurityRt;
use http::{HeaderMap, HeaderValue};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(value) = HeaderValue::from_bytes(data) else { return };
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    for header in ["x-forwarded-for", "forwarded"] {
        let doc = serde_json::json!({"security": {"client_ip": {
            "trusted_proxies": ["0.0.0.0/0", "::/0"], "header": header}}});
        let Ok(security) = SecurityRt::compile(&doc) else { return };
        let mut headers = HeaderMap::new();
        headers.insert(header, value.clone());
        let _ = security.client_ip.resolve(peer, &headers);
    }
});

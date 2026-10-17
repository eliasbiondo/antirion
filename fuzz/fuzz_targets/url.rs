#![no_main]

use antirion_egress::ssrf::{EgressPolicy, TrustClass, validate_url};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else { return };
    let policy = EgressPolicy::default();
    for class in [TrustClass::Operator, TrustClass::ClientInfluenced] {
        let _ = validate_url(raw, class, &policy);
    }
});

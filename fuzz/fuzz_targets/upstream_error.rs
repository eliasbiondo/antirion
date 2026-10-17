#![no_main]

use libfuzzer_sys::fuzz_target;

// The first two bytes are the status; the third picks the content type.
fuzz_target!(|data: &[u8]| {
    let [hi, lo, ct, body @ ..] = data else { return };
    let status = 400 + u16::from_be_bytes([*hi, *lo]) % 200;
    let content_type = match ct % 3 {
        0 => Some("application/json"),
        1 => Some("text/event-stream"),
        _ => None,
    };
    let _ = antirion_pipeline::fuzzing::upstream_error(status, content_type, body);
});

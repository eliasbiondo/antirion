#![no_main]

use antirion_config::{Format, parse};
use libfuzzer_sys::fuzz_target;

// A configuration document in either format, then its validation as a whole.
fuzz_target!(|data: &[u8]| {
    let Some((&format, body)) = data.split_first() else { return };
    let format = if format % 2 == 0 { Format::Yaml } else { Format::Json };
    if let Ok(mut doc) = parse(body, format) {
        let _ = antirion_config::validate(&mut doc, &serde_json::Value::Null);
    }
});

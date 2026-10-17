#![no_main]

use antirion_wire::sse::SseParser;
use libfuzzer_sys::fuzz_target;

// The first byte splits the rest into two reads, so events crossing a read boundary are covered.
fuzz_target!(|data: &[u8]| {
    let Some((&split, rest)) = data.split_first() else { return };
    let at = usize::from(split).min(rest.len());
    let (a, b) = rest.split_at(at);
    for mut parser in [SseParser::new(64 * 1024), SseParser::new(64 * 1024).with_bare_errors()] {
        let mut events = Vec::new();
        if parser.push(a, &mut events).is_ok() && parser.push(b, &mut events).is_ok() {
            let _ = parser.finish();
        }
    }
});

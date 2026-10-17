#![no_main]

use antirion_wire::eventstream::Decoder;
use antirion_wire::jsonstream::{JsonArrayParser, NdjsonParser};
use libfuzzer_sys::fuzz_target;

// NDJSON, a streamed JSON array and the AWS event stream, each fed in two reads.
fuzz_target!(|data: &[u8]| {
    let Some((&split, rest)) = data.split_first() else { return };
    let at = usize::from(split).min(rest.len());
    let (a, b) = rest.split_at(at);
    let mut events = Vec::new();
    let mut ndjson = NdjsonParser::new(64 * 1024);
    if ndjson.push(a, &mut events).is_ok() {
        let _ = ndjson.push(b, &mut events);
    }
    let mut array = JsonArrayParser::new(64 * 1024);
    if array.push(a, &mut events).is_ok() {
        let _ = array.push(b, &mut events);
    }
    let mut messages = Vec::new();
    let mut decoder = Decoder::new(64 * 1024);
    if decoder.push(a, &mut messages).is_ok() {
        let _ = decoder.push(b, &mut messages);
    }
});

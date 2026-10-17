#![no_main]

use antirion_dialects::xlate::ir::Client;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    antirion_fuzz::request(Client::Gemini, "/v1beta/models/m:generateContent", data);
});

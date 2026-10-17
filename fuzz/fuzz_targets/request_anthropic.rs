#![no_main]

use antirion_dialects::xlate::ir::Client;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    antirion_fuzz::request(Client::Anthropic, "/v1/messages", data);
});

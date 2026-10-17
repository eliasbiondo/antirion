//! The extensions shipped in the binary: `static_response` in every build, and `test_script`, which
//! the binary registers only in test mode.

mod test_script;

use crate::api::{Extension, Failure, Hook, HookAction, Input, Reply};

pub use test_script::TestScript;

pub(crate) const STATIC_RESPONSE: &str = "static_response";

/// The name `TestScript` is registered under in test mode.
pub const TEST_SCRIPT: &str = "test_script";

/// Answers every request with `config.content` and `config.finish_reason` (default `stop`).
#[derive(Debug)]
pub(crate) struct StaticResponse;

impl Extension for StaticResponse {
    fn hooks(&self) -> &[Hook] {
        &[Hook::Request]
    }

    fn call(&self, input: &Input<'_>) -> Result<HookAction, Failure> {
        let text = |k: &str| input.config.get(k).and_then(serde_json::Value::as_str);
        Ok(HookAction::Respond(Reply {
            content: text("content").unwrap_or_default().to_owned(),
            finish_reason: text("finish_reason").unwrap_or("stop").to_owned(),
            input: 0,
            output: 0,
        }))
    }
}

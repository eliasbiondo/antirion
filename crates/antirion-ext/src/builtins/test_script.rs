//! `test_script`: a test-mode extension whose `config` scripts its answer for each hook, in the
//! webhook answer format, so tests can drive in-process hooks.
//!
//! ```yaml
//! config:
//!   request: {action: continue}          # the answer of every request hook call
//!   response: {action: continue}
//!   attempt: {action: mutate, set_headers: {x-attempt: "{index}"}}   # {index}: the attempt index
//!   stream_chunk: {on: 3, answer: {action: deny, status: 403, message: "no"}}   # the 3rd event
//!   stream_chunk: {every: {action: mutate, body_patch: [...]}}                   # every event
//!   panic: [request]                      # these hooks panic
//! ```

use serde_json::Value;

use crate::api::{Extension, Failure, Hook, HookAction, Input};
use crate::hook::parse_answer;

/// The scripted test extension.
#[derive(Debug)]
pub struct TestScript;

const HOOKS: &[Hook] = &[Hook::Request, Hook::Response, Hook::StreamChunk, Hook::Attempt];

fn answer(hook: Hook, v: &Value) -> Result<HookAction, Failure> {
    let bytes = serde_json::to_vec(v).map_err(|e| Failure::new(e.to_string()))?;
    parse_answer(hook, 200, &bytes).map_err(Failure::new)
}

/// `{index}` in header values becomes the attempt index.
fn with_index(v: &Value, index: u32) -> Value {
    match v {
        Value::String(s) => Value::String(s.replace("{index}", &index.to_string())),
        Value::Array(a) => Value::Array(a.iter().map(|x| with_index(x, index)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), with_index(x, index))).collect()),
        other => other.clone(),
    }
}

impl Extension for TestScript {
    fn hooks(&self) -> &[Hook] {
        HOOKS
    }

    #[allow(clippy::panic)]
    fn call(&self, input: &Input<'_>) -> Result<HookAction, Failure> {
        let hook = input.hook;
        let panics = input
            .config
            .get("panic")
            .and_then(Value::as_array)
            .is_some_and(|l| l.iter().any(|h| h.as_str() == Some(hook.as_str())));
        if panics {
            panic!("test_script panics in the {} hook as configured", hook.as_str());
        }
        let Some(script) = input.config.get(hook.as_str()) else {
            return Ok(HookAction::Continue);
        };
        match hook {
            Hook::StreamChunk => {
                let index = input.event.map_or(0, |e| e.index);
                if let Some(every) = script.get("every") {
                    return answer(hook, every);
                }
                match (script.get("on").and_then(Value::as_u64), script.get("answer")) {
                    (Some(on), Some(a)) if on == index => answer(hook, a),
                    _ => Ok(HookAction::Continue),
                }
            }
            Hook::Attempt => answer(hook, &with_index(script, input.attempt.map_or(0, |a| a.index))),
            Hook::Request | Hook::Response => answer(hook, script),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::api::{AttemptTarget, Event};
    use crate::hook::HookFacts;

    #[test]
    fn answers_follow_the_script() {
        let facts = HookFacts::default();
        let config = json!({
            "stream_chunk": {"on": 3, "answer": {"action": "deny", "status": 403, "message": "third"}},
            "attempt": {"action": "mutate", "set_headers": {"x-attempt": "n{index}"}}
        });
        let data = json!({});
        let at = |i| {
            let input =
                Input::new(Hook::StreamChunk, "t", &config, &facts).with_event(Event::new(i, None, &data));
            TestScript.call(&input)
        };
        assert_eq!(at(2), Ok(HookAction::Continue));
        assert!(matches!(at(3), Ok(HookAction::Deny { status: 403, .. })));
        let target = AttemptTarget::new(2, "d", "p", "openai", "m");
        let input = Input::new(Hook::Attempt, "t", &config, &facts).with_attempt(&target);
        match TestScript.call(&input) {
            Ok(HookAction::Mutate { set_headers, .. }) => {
                assert_eq!(set_headers, [("x-attempt".to_owned(), "n2".to_owned())]);
            }
            other => panic!("{other:?}"),
        }
        let input = Input::new(Hook::Request, "t", &config, &facts);
        assert_eq!(TestScript.call(&input), Ok(HookAction::Continue));
    }
}

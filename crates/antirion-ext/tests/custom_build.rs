//! A custom build's view of the in-process API: it registers its own extension before the gateway
//! starts, and configurations name it as `type: builtin`.

use std::sync::Arc;

use antirion_ext::api::{
    Event, Extension, Failure, Hook, HookAction, HookFacts, Input, Registry, call_guarded, install,
};
use serde_json::{Value, json};

#[derive(Debug)]
struct Shout;

impl Extension for Shout {
    fn hooks(&self) -> &[Hook] {
        &[Hook::Request, Hook::StreamChunk]
    }

    fn call(&self, input: &Input<'_>) -> Result<HookAction, Failure> {
        let Some(event) = input.event else {
            return Ok(HookAction::Continue);
        };
        let text = event
            .data
            .pointer("/delta/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(HookAction::Mutate {
            patch: vec![json!({"op": "replace", "path": "/delta/text", "value": text.to_uppercase()})],
            set_headers: Vec::new(),
            metadata: serde_json::Map::new(),
        })
    }
}

#[test]
fn a_registered_extension_is_configured_by_name() {
    assert!(install(Registry::builtin().with("shout", Arc::new(Shout))));
    assert!(!install(Registry::builtin()), "only the first registry counts");
    let doc = json!({"plugins": {"s": {"type": "builtin", "builtin": {"name": "shout"}, "hooks": ["stream_chunk"]}}});
    let plugins = antirion_ext::compile(&doc).unwrap();
    let s = &plugins.by_name["s"];
    assert!(s.runs(Hook::StreamChunk) && !s.runs(Hook::Request));
    let undeclared =
        json!({"plugins": {"s": {"type": "builtin", "builtin": {"name": "shout"}, "hooks": ["attempt"]}}});
    assert_eq!(
        antirion_ext::compile(&undeclared).unwrap_err()[0].path,
        "plugins.s.hooks[0]"
    );
    let antirion_ext::Carrier::Builtin(ext) = &s.carrier else {
        panic!("a registered extension is a built-in carrier");
    };
    let facts = HookFacts::default();
    let data = json!({"type": "content_block_delta", "delta": {"text": "quiet"}});
    let input = Input::new(Hook::StreamChunk, "s", &s.config, &facts).with_event(Event::new(1, None, &data));
    let HookAction::Mutate { patch, .. } = call_guarded(ext.as_ref(), &input).unwrap() else {
        panic!("the extension mutates the event");
    };
    let patched = antirion_ext::patch::apply(&data, &patch).unwrap();
    assert_eq!(patched["delta"]["text"], "QUIET");
}

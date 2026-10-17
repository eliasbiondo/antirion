//! The in-process extension API: what a custom build implements to add its own extensions,
//! referenced in the configuration as `type: builtin` with `builtin.name` set to the name they are
//! registered under.
//!
//! This module is the crate's stable surface. Within one major version of `antirion-ext` (one minor
//! version while the version is `0.x`) its items only change in compatible ways: new hooks, new
//! fields of the `#[non_exhaustive]` types and new provided trait methods may appear; nothing is
//! removed or changes meaning. Everything outside this module may change in any release.
//!
//! ```
//! use std::sync::Arc;
//! use antirion_ext::api::{Extension, Failure, Hook, HookAction, Input, Registry};
//!
//! #[derive(Debug)]
//! struct Tagger;
//!
//! impl Extension for Tagger {
//!     fn hooks(&self) -> &[Hook] {
//!         &[Hook::Request]
//!     }
//!
//!     fn call(&self, input: &Input<'_>) -> Result<HookAction, Failure> {
//!         let mut metadata = serde_json::Map::new();
//!         metadata.insert("tagged_by".into(), input.extension.into());
//!         Ok(HookAction::Mutate { patch: Vec::new(), set_headers: Vec::new(), metadata })
//!     }
//! }
//!
//! let registry = Registry::builtin().with("tagger", Arc::new(Tagger));
//! assert!(registry.get("tagger").is_some());
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use serde_json::Value;

pub use crate::hook::{HookAction, HookFacts, Reply};
pub use crate::plugin::Hook;

/// One streamed event as a `stream_chunk` hook sees it: the event of the client's dialect.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Event<'a> {
    /// The event's position in the stream, from 1.
    pub index: u64,
    /// The server-sent event name, when the dialect names its events.
    pub name: Option<&'a str>,
    /// The event's JSON payload; a `mutate` patch applies to it.
    pub data: &'a Value,
}

impl<'a> Event<'a> {
    #[must_use]
    pub fn new(index: u64, name: Option<&'a str>, data: &'a Value) -> Event<'a> {
        Event { index, name, data }
    }
}

/// Where an `attempt` hook's upstream request goes.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct AttemptTarget {
    /// The attempt's position in the request, from 0 (retries and fallbacks count).
    pub index: u32,
    pub deployment: String,
    pub provider: String,
    /// The provider's type (`openai`, `anthropic`, …).
    pub provider_type: String,
    /// The model id the deployment sends upstream.
    pub model: String,
}

impl AttemptTarget {
    #[must_use]
    pub fn new(
        index: u32,
        deployment: &str,
        provider: &str,
        provider_type: &str,
        model: &str,
    ) -> AttemptTarget {
        AttemptTarget {
            index,
            deployment: deployment.to_owned(),
            provider: provider.to_owned(),
            provider_type: provider_type.to_owned(),
            model: model.to_owned(),
        }
    }
}

/// What one hook call sees: the request's facts and the part of the exchange its hook is about.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Input<'a> {
    pub hook: Hook,
    /// The name the extension is configured under.
    pub extension: &'a str,
    /// The extension's `config` block.
    pub config: &'a Value,
    pub facts: &'a HookFacts,
    /// `request`: the client's body; `attempt`: the upstream body. Absent with `include_body:
    /// false` or for a body that is not JSON.
    pub body: Option<&'a Value>,
    /// `response`: the status and the body in the client's dialect.
    pub response: Option<(u16, &'a Value)>,
    /// `stream_chunk`: the event.
    pub event: Option<Event<'a>>,
    /// `attempt`: which attempt, and where it goes.
    pub attempt: Option<&'a AttemptTarget>,
}

impl<'a> Input<'a> {
    #[must_use]
    pub fn new(hook: Hook, extension: &'a str, config: &'a Value, facts: &'a HookFacts) -> Input<'a> {
        Input {
            hook,
            extension,
            config,
            facts,
            body: None,
            response: None,
            event: None,
            attempt: None,
        }
    }

    #[must_use]
    pub fn with_body(mut self, body: Option<&'a Value>) -> Input<'a> {
        self.body = body;
        self
    }

    #[must_use]
    pub fn with_response(mut self, status: u16, body: &'a Value) -> Input<'a> {
        self.response = Some((status, body));
        self
    }

    #[must_use]
    pub fn with_event(mut self, event: Event<'a>) -> Input<'a> {
        self.event = Some(event);
        self
    }

    #[must_use]
    pub fn with_attempt(mut self, attempt: &'a AttemptTarget) -> Input<'a> {
        self.attempt = Some(attempt);
        self
    }
}

/// A hook call that failed: the extension's `on_error` decides what the request does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure(String);

impl Failure {
    pub fn new(message: impl Into<String>) -> Failure {
        Failure(message.into())
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Failure {}

/// An extension compiled into the gateway.
///
/// `call` runs on the request's task, so it must not block; the gateway measures each call against
/// the extension's `timeout` and treats one that returns late as timed out. A panic is caught and is
/// a hook failure; it never ends the process or another request.
pub trait Extension: Send + Sync + fmt::Debug + 'static {
    /// The hooks the extension can run. A configuration that declares another one is refused.
    fn hooks(&self) -> &[Hook];

    /// Decides one hook call. Actions a hook does not allow (a `respond` outside `request`, a
    /// `deny` in `attempt`) are failures.
    fn call(&self, input: &Input<'_>) -> Result<HookAction, Failure>;
}

/// Calls `ext`, turning a panic into a failure.
pub fn call_guarded(ext: &dyn Extension, input: &Input<'_>) -> Result<HookAction, Failure> {
    match catch_unwind(AssertUnwindSafe(|| ext.call(input))) {
        Ok(r) => r,
        Err(_) => Err(Failure::new("the extension panicked")),
    }
}

/// The extensions a build offers as `type: builtin`, by name.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    by_name: BTreeMap<String, Arc<dyn Extension>>,
}

impl Registry {
    /// The extensions every build ships (`static_response`).
    #[must_use]
    pub fn builtin() -> Registry {
        Registry::default().with(
            crate::builtins::STATIC_RESPONSE,
            Arc::new(crate::builtins::StaticResponse),
        )
    }

    /// Adds (or replaces) the extension registered as `name`.
    #[must_use]
    pub fn with(mut self, name: &str, ext: Arc<dyn Extension>) -> Registry {
        self.by_name.insert(name.to_owned(), ext);
        self
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Extension>> {
        self.by_name.get(name)
    }

    /// The registered names, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }
}

/// Makes `registry` the one this process compiles `type: builtin` extensions against. Only the
/// first call has an effect; it returns whether this was it. Call it before the gateway starts.
pub fn install(registry: Registry) -> bool {
    crate::registry::install(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Panics;

    impl Extension for Panics {
        fn hooks(&self) -> &[Hook] {
            &[Hook::Request]
        }

        #[allow(clippy::panic)]
        fn call(&self, _: &Input<'_>) -> Result<HookAction, Failure> {
            panic!("boom")
        }
    }

    #[test]
    fn a_panic_is_a_failure() {
        let facts = HookFacts::default();
        let config = Value::Null;
        let input = Input::new(Hook::Request, "p", &config, &facts);
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r = call_guarded(&Panics, &input);
        std::panic::set_hook(prev);
        assert_eq!(r, Err(Failure::new("the extension panicked")));
    }

    #[test]
    fn the_builtin_registry_has_static_response() {
        let r = Registry::builtin();
        assert_eq!(r.names().collect::<Vec<_>>(), ["static_response"]);
        assert!(r.get("nope").is_none());
    }
}

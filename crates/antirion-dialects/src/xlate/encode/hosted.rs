//! Hosted tools with an equivalent in the target: web search as Chat `web_search_options`, a
//! Responses `web_search` tool, Anthropic's `web_search` server tool or Gemini `googleSearch`.

use serde_json::{Map, Value, json};

use super::Enc;
use crate::xlate::ir::{Location, WebSearch};
use crate::xlate::target::Upstream;
use crate::xlate::util::put;
use crate::xlate::warn::Kind;

impl<'a> Enc<'a> {
    /// The client's web search when the target has an equivalent (reported approximated);
    /// reported dropped otherwise.
    pub(super) fn web_search(&mut self) -> Option<&'a WebSearch> {
        let w = self.req.web_search.as_ref()?;
        let mapped = matches!(
            self.dialect(),
            Upstream::Chat | Upstream::Responses | Upstream::Anthropic | Upstream::Gemini
        );
        self.warn(if mapped { Kind::Approximated } else { Kind::Dropped }, &w.src);
        mapped.then_some(w)
    }
}

fn flat(l: &Location) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("type".into(), "approximate".into());
    for (k, v) in [
        ("city", &l.city),
        ("region", &l.region),
        ("country", &l.country),
        ("timezone", &l.timezone),
    ] {
        put(&mut m, k, v.clone().map_or(Value::Null, Value::from));
    }
    m
}

/// Chat `web_search_options`.
pub(super) fn chat_options(w: &WebSearch) -> Value {
    let mut o = Map::new();
    put(
        &mut o,
        "search_context_size",
        w.context_size.clone().map_or(Value::Null, Value::from),
    );
    if let Some(l) = &w.location {
        let mut inner = flat(l);
        inner.shift_remove("type");
        o.insert(
            "user_location".into(),
            json!({"type": "approximate", "approximate": inner}),
        );
    }
    Value::Object(o)
}

/// A Responses `web_search` tool.
pub(super) fn responses_tool(w: &WebSearch) -> Value {
    let mut o = Map::new();
    o.insert("type".into(), "web_search".into());
    put(
        &mut o,
        "search_context_size",
        w.context_size.clone().map_or(Value::Null, Value::from),
    );
    if let Some(l) = &w.location {
        o.insert("user_location".into(), Value::Object(flat(l)));
    }
    if !w.allowed_domains.is_empty() {
        o.insert("filters".into(), json!({"allowed_domains": w.allowed_domains}));
    }
    Value::Object(o)
}

/// Anthropic's `web_search` server tool.
pub(super) fn anthropic_tool(w: &WebSearch) -> Value {
    let mut o = Map::new();
    o.insert("type".into(), "web_search_20250305".into());
    o.insert("name".into(), "web_search".into());
    put(&mut o, "max_uses", w.max_uses.map_or(Value::Null, Value::from));
    if !w.allowed_domains.is_empty() {
        o.insert("allowed_domains".into(), json!(w.allowed_domains));
    } else if !w.blocked_domains.is_empty() {
        o.insert("blocked_domains".into(), json!(w.blocked_domains));
    }
    if let Some(l) = &w.location {
        o.insert("user_location".into(), Value::Object(flat(l)));
    }
    Value::Object(o)
}

/// Gemini's search tool.
pub(super) fn gemini_tool() -> Value {
    json!({"googleSearch": {}})
}

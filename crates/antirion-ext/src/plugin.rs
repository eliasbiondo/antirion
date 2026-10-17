//! Compiled extensions (`plugins`), their attachment to a request, and the configuration rules the
//! schema cannot express.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::Value;

use crate::api::Extension;

/// A hook point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Hook {
    /// Before the input guardrails, with the client's request.
    Request,
    /// After the output guardrails, with a complete response.
    Response,
    /// Per event of a streamed response, after the output guardrails.
    StreamChunk,
    /// Before each upstream send, with the upstream request.
    Attempt,
}

impl Hook {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Hook::Request => "request",
            Hook::Response => "response",
            Hook::StreamChunk => "stream_chunk",
            Hook::Attempt => "attempt",
        }
    }

    /// The hook a configuration names.
    #[must_use]
    pub fn parse(name: &str) -> Option<Hook> {
        match name {
            "request" => Some(Hook::Request),
            "response" => Some(Hook::Response),
            "stream_chunk" => Some(Hook::StreamChunk),
            "attempt" => Some(Hook::Attempt),
            _ => None,
        }
    }

    const fn bit(self) -> u8 {
        match self {
            Hook::Request => 1,
            Hook::Response => 2,
            Hook::StreamChunk => 4,
            Hook::Attempt => 8,
        }
    }
}

/// How an extension is called.
#[derive(Clone, Debug)]
pub enum Carrier {
    Webhook {
        url: String,
        headers: Vec<(String, String)>,
        signing_secret: Option<antirion_core::Secret>,
        max_response_bytes: u64,
        /// Lowercase names of client headers sent in the payload.
        forward_headers: Vec<String>,
    },
    /// An extension compiled into the binary (`type: builtin`).
    Builtin(Arc<dyn Extension>),
    /// A Wasm component (`type: wasm`).
    Wasm(Arc<crate::wasm::Module>),
}

/// One compiled extension.
#[derive(Clone, Debug)]
pub struct Plugin {
    pub name: String,
    pub carrier: Carrier,
    /// The hooks it runs, as bits of [`Hook`].
    hooks: u8,
    pub order: i64,
    pub fail_open: bool,
    pub timeout: Duration,
    pub include_body: bool,
    pub config: Value,
}

impl Plugin {
    /// Whether the extension is attached to this hook.
    #[must_use]
    pub fn runs(&self, hook: Hook) -> bool {
        self.hooks & hook.bit() != 0
    }
}

/// Every enabled extension by name.
#[derive(Clone, Debug, Default)]
pub struct Plugins {
    pub by_name: BTreeMap<String, Arc<Plugin>>,
}

impl Plugins {
    /// Whether some extension is a Wasm component (its calls need the epoch clock).
    #[must_use]
    pub fn has_wasm(&self) -> bool {
        self.by_name
            .values()
            .any(|p| matches!(p.carrier, Carrier::Wasm(_)))
    }
}

const WEBHOOK_HOOKS: &[Hook] = &[Hook::Request, Hook::Response, Hook::Attempt];
const WASM_HOOKS: &[Hook] = &[Hook::Request, Hook::Response, Hook::StreamChunk, Hook::Attempt];
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(500);
const DEFAULT_MAX_RESPONSE: u64 = 1 << 20;

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Compiles `plugins` (secret references resolved), loading and compiling Wasm components; it
/// reads module files, so it runs off the request path.
pub fn compile(doc: &Value) -> Result<Plugins, Vec<ConfigError>> {
    let mut errors = Vec::new();
    let p = compile_into(doc, &mut errors, true);
    if errors.is_empty() { Ok(p) } else { Err(errors) }
}

fn compile_into(doc: &Value, errors: &mut Vec<ConfigError>, load: bool) -> Plugins {
    let mut by_name = BTreeMap::new();
    let root = DocPath::root().key("plugins");
    for (name, p) in doc
        .get("plugins")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let at = root.key(name);
        if let Some(plugin) = plugin(name, p, &at, errors, load)
            && p.get("enabled").and_then(Value::as_bool) != Some(false)
        {
            by_name.insert(name.clone(), Arc::new(plugin));
        }
    }
    Plugins { by_name }
}

fn plugin(name: &str, p: &Value, at: &DocPath, errors: &mut Vec<ConfigError>, load: bool) -> Option<Plugin> {
    let ty = s(p, "type")?;
    let carrier = match ty {
        "webhook" => {
            let w = p.get("webhook")?;
            Carrier::Webhook {
                url: s(w, "url")?.to_owned(),
                headers: w
                    .get("headers")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                    .collect(),
                signing_secret: s(w, "signing_secret").map(antirion_core::Secret::new),
                max_response_bytes: s(w, "max_response_bytes")
                    .and_then(antirion_config::scalar::parse_size)
                    .unwrap_or(DEFAULT_MAX_RESPONSE),
                forward_headers: w
                    .get("forward_headers")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_ascii_lowercase)
                    .collect(),
            }
        }
        "builtin" => {
            let builtin = s(p.get("builtin")?, "name")?;
            let Some(ext) = crate::registry::current().get(builtin) else {
                errors.push(ConfigError::new(
                    &at.key("builtin").key("name"),
                    DetailCode::InvalidEnum,
                    format!("{builtin:?} is not a built-in extension"),
                ));
                return None;
            };
            Carrier::Builtin(Arc::clone(ext))
        }
        "wasm" => {
            let w = p.get("wasm")?;
            let source = crate::wasm::Source::from_block(w);
            if !load {
                Carrier::Wasm(Arc::new(crate::wasm::Module::unloaded(source)))
            } else {
                match crate::wasm::Module::load(source) {
                    Ok(m) => Carrier::Wasm(Arc::new(m)),
                    Err(e) => {
                        errors.push(ConfigError::new(
                            &at.key("wasm").key(e.field()),
                            e.code(),
                            e.to_string(),
                        ));
                        return None;
                    }
                }
            }
        }
        _ => {
            errors.push(ConfigError::new(
                &at.key("type"),
                DetailCode::InvalidEnum,
                format!("extension type {ty:?} is not supported in this build"),
            ));
            return None;
        }
    };
    let supported: &[Hook] = match &carrier {
        Carrier::Webhook { .. } => WEBHOOK_HOOKS,
        Carrier::Builtin(ext) => ext.hooks(),
        Carrier::Wasm(_) => WASM_HOOKS,
    };
    let mut hooks = 0u8;
    match p.get("hooks").and_then(Value::as_array) {
        Some(declared) => {
            for (i, h) in declared.iter().enumerate() {
                match h.as_str().and_then(Hook::parse) {
                    Some(hook) if supported.contains(&hook) => hooks |= hook.bit(),
                    _ => errors.push(ConfigError::new(
                        &at.key("hooks").index(i),
                        DetailCode::NotSupportedInThisBuild,
                        format!("extension {name:?} cannot run this hook"),
                    )),
                }
            }
        }
        None => {
            for hook in supported {
                hooks |= hook.bit();
            }
        }
    }
    if let Carrier::Wasm(m) = &carrier {
        for hook in WASM_HOOKS.iter().filter(|h| hooks & h.bit() != 0) {
            if let Err(e) = m.check_export(*hook) {
                errors.push(ConfigError::new(
                    &at.key("wasm").key(e.field()),
                    e.code(),
                    e.to_string(),
                ));
            }
        }
    }
    Some(Plugin {
        name: name.to_owned(),
        carrier,
        hooks,
        order: p.get("order").and_then(Value::as_i64).unwrap_or(100),
        fail_open: s(p, "on_error") == Some("fail_open"),
        timeout: s(p, "timeout")
            .and_then(antirion_config::scalar::parse_duration)
            .unwrap_or(DEFAULT_TIMEOUT),
        include_body: p.get("include_body").and_then(Value::as_bool) != Some(false),
        config: p
            .get("config")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
    })
}

/// The configuration errors of the document's extensions.
#[must_use]
pub fn check_document(doc: &Value) -> Vec<ConfigError> {
    let mut errors = Vec::new();
    compile_into(doc, &mut errors, false);
    errors
}

fn attach_name(item: &Value) -> Option<&str> {
    item.as_str().or_else(|| item.get("name").and_then(Value::as_str))
}

/// The extensions a request runs, in `request`-hook order: attachments accumulated from the
/// broadest level (`attach` adds, `detach` removes), then by level, `order` and name.
#[must_use]
pub fn resolve(plugins: &Plugins, levels: &[Option<&Value>]) -> Vec<Arc<Plugin>> {
    let mut entries: Vec<(usize, &str)> = Vec::new();
    for (level, block) in levels.iter().enumerate() {
        let Some(block) = block else { continue };
        for d in block
            .get("detach")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(n) = d.as_str() {
                entries.retain(|(_, e)| *e != n);
            }
        }
        for a in block
            .get("attach")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(n) = attach_name(a)
                && !entries.iter().any(|(_, e)| *e == n)
            {
                entries.push((level, n));
            }
        }
    }
    let mut out: Vec<(usize, Arc<Plugin>)> = entries
        .into_iter()
        .filter_map(|(l, n)| plugins.by_name.get(n).map(|p| (l, Arc::clone(p))))
        .collect();
    out.sort_by(|(la, a), (lb, b)| la.cmp(lb).then(a.order.cmp(&b.order)).then(a.name.cmp(&b.name)));
    out.into_iter().map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn hooks_each_carrier_runs() {
        let doc = json!({"plugins": {
            "x": {"type": "webhook", "hooks": ["request", "stream_chunk"], "webhook": {"url": "https://h"}},
            "a": {"type": "webhook", "hooks": ["attempt"], "webhook": {"url": "https://h"}},
            "b": {"type": "builtin", "builtin": {"name": "nope"}},
            "s": {"type": "builtin", "hooks": ["response"], "builtin": {"name": "static_response"}, "config": {"content": "c"}},
            "w": {"type": "wasm", "hooks": ["stream_chunk"], "wasm": {"module": "file:///x.wasm", "sha256": "00"}}
        }});
        let mut paths: Vec<String> = check_document(&doc).into_iter().map(|e| e.path).collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                "plugins.b.builtin.name",
                "plugins.s.hooks[0]",
                "plugins.x.hooks[1]"
            ]
        );
        let compiled = compile_into(&doc, &mut Vec::new(), false);
        let a = &compiled.by_name["a"];
        assert!(a.runs(Hook::Attempt) && !a.runs(Hook::Request));
        let x = &compiled.by_name["x"];
        assert!(x.runs(Hook::Request) && !x.runs(Hook::StreamChunk));
        assert!(compiled.has_wasm());
    }

    #[test]
    fn a_wasm_module_that_cannot_be_read_is_named() {
        let doc = json!({"plugins": {
            "w": {"type": "wasm", "wasm": {"module": "file:///nonexistent/antirion.wasm", "sha256": "0".repeat(64)}}
        }});
        let errors = compile(&doc).unwrap_err();
        assert_eq!(errors[0].path, "plugins.w.wasm.module");
        assert_eq!(errors[0].code, DetailCode::UnresolvedReference);
    }

    #[test]
    fn order_is_level_then_order_then_name() {
        let doc = json!({"plugins": {
            "g_late": {"type": "webhook", "order": 50, "webhook": {"url": "https://h"}},
            "m_first": {"type": "webhook", "order": -10, "webhook": {"url": "https://h"}},
            "m_b": {"type": "webhook", "order": 1, "webhook": {"url": "https://h"}},
            "m_a": {"type": "webhook", "order": 1, "webhook": {"url": "https://h"}}
        }});
        let p = compile(&doc).unwrap();
        let gateway = json!({"attach": ["g_late"]});
        let model = json!({"attach": ["m_b", "m_first", {"name": "m_a"}]});
        let names: Vec<String> = resolve(&p, &[Some(&gateway), Some(&model)])
            .iter()
            .map(|p| p.name.clone())
            .collect();
        assert_eq!(names, ["g_late", "m_first", "m_a", "m_b"]);
        let route = json!({"detach": ["g_late"]});
        assert_eq!(resolve(&p, &[Some(&gateway), Some(&route)]).len(), 0);
    }
}

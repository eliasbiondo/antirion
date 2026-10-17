//! Wasm components (`type: wasm`): WASI 0.2 components exporting the `antirion:extension/handler`
//! interface of `wit/hooks.wit`, one function per hook, each taking the hook's JSON payload and
//! returning the JSON answer.
//!
//! A module is read and checked against its `sha256` when a configuration is compiled, compiled
//! once (and kept by digest, so an unrelated configuration change reuses it) and linked ahead; each
//! call instantiates it in a store of its own, so nothing is shared between calls. A store's memory
//! is capped at `max_memory`, and its time at `max_duration` through epoch interruption: the
//! runtime advances the epoch every [`TICK`] while a component is configured. WASI gives the
//! component no files, no environment, no arguments and no network.

mod fetch;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use antirion_config::DetailCode;
use serde_json::Value;
use sha2::{Digest, Sha256};
use wasmtime::component::{Component, ComponentExportIndex, InstancePre, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

pub use fetch::{OciRef, fetched, provide};

use crate::api::Failure;
use crate::plugin::Hook;

/// The epoch clock's period; a call's time limit is counted in these ticks.
pub const TICK: Duration = Duration::from_millis(1);

const INTERFACE: &str = "antirion:extension/handler@1.0.0";
const DEFAULT_MEMORY: usize = 64 << 20;
const DEFAULT_DURATION: Duration = Duration::from_millis(5);
/// Compiled components kept beyond this many are dropped.
const MAX_COMPILED: usize = 64;

/// Where a module comes from and the limits of its calls.
#[derive(Clone, Debug)]
pub struct Source {
    pub module: String,
    /// The expected SHA-256 of the module, lowercase hex.
    pub sha256: String,
    pub max_memory: usize,
    pub max_duration: Duration,
}

impl Source {
    /// The source of a `wasm` block.
    #[must_use]
    pub fn from_block(w: &Value) -> Source {
        let text = |k: &str| w.get(k).and_then(Value::as_str);
        Source {
            module: text("module").unwrap_or_default().to_owned(),
            sha256: text("sha256").unwrap_or_default().to_ascii_lowercase(),
            max_memory: text("max_memory")
                .and_then(antirion_config::scalar::parse_size)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(DEFAULT_MEMORY),
            max_duration: text("max_duration")
                .and_then(antirion_config::scalar::parse_duration)
                .unwrap_or(DEFAULT_DURATION),
        }
    }

    /// The epoch ticks a call may run: its time limit, rounded up, plus the tick in progress.
    fn ticks(&self) -> u64 {
        let tick = TICK.as_nanos().max(1);
        u64::try_from(self.max_duration.as_nanos().div_ceil(tick))
            .unwrap_or(u64::MAX)
            .saturating_add(1)
    }
}

/// Why a module cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("the module cannot be read: {0}")]
    Unreadable(String),
    #[error("the module's SHA-256 is {actual}, not the configured {expected}")]
    Digest { expected: String, actual: String },
    #[error("the module has not been fetched from its registry")]
    NotFetched,
    #[error("the module is not a usable component: {0}")]
    Invalid(String),
    #[error("the component does not export the {0} function of {INTERFACE}")]
    MissingExport(&'static str),
    #[error("this platform cannot run Wasm components: {0}")]
    Engine(String),
}

impl LoadError {
    /// The member of the `wasm` block the error is about.
    #[must_use]
    pub fn field(&self) -> &'static str {
        match self {
            LoadError::Digest { .. } => "sha256",
            _ => "module",
        }
    }

    #[must_use]
    pub fn code(&self) -> DetailCode {
        match self {
            LoadError::Unreadable(_) | LoadError::NotFetched => DetailCode::UnresolvedReference,
            LoadError::Digest { .. } => DetailCode::Conflict,
            LoadError::Invalid(_) | LoadError::MissingExport(_) | LoadError::Engine(_) => {
                DetailCode::InvalidFormat
            }
        }
    }
}

struct Host {
    ctx: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
}

impl WasiView for Host {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

struct Loaded {
    pre: InstancePre<Host>,
    /// The export of each hook, in [`hook_slot`] order.
    exports: [Option<ComponentExportIndex>; 4],
}

/// A Wasm extension's module, compiled and linked.
pub struct Module {
    source: Source,
    loaded: Option<Loaded>,
}

impl std::fmt::Debug for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Module")
            .field("module", &self.source.module)
            .field("sha256", &self.source.sha256)
            .field("loaded", &self.loaded.is_some())
            .finish()
    }
}

fn engine() -> Result<&'static Engine, LoadError> {
    static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let mut config = Config::new();
            config.wasm_component_model(true).epoch_interruption(true);
            Engine::new(&config).map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| LoadError::Engine(e.clone()))
}

/// Advances the epoch clock by one tick.
pub fn tick() {
    if let Ok(e) = engine() {
        e.increment_epoch();
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SHA-256 of `bytes`, lowercase hex.
#[must_use]
pub fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

const fn hook_slot(hook: Hook) -> usize {
    match hook {
        Hook::Request => 0,
        Hook::Response => 1,
        Hook::StreamChunk => 2,
        Hook::Attempt => 3,
    }
}

const FUNCTIONS: [&str; 4] = ["request", "response", "stream-chunk", "attempt"];

const fn function(hook: Hook) -> &'static str {
    match hook {
        Hook::Request => "request",
        Hook::Response => "response",
        Hook::StreamChunk => "stream-chunk",
        Hook::Attempt => "attempt",
    }
}

fn export_of(l: &Loaded, hook: Hook) -> Option<ComponentExportIndex> {
    l.exports.get(hook_slot(hook)).copied().flatten()
}

fn read(source: &Source) -> Result<Arc<[u8]>, LoadError> {
    if let Some(path) = source.module.strip_prefix("file://") {
        return std::fs::read(path)
            .map(Arc::from)
            .map_err(|e| LoadError::Unreadable(format!("{path}: {e}")));
    }
    fetch::bytes(&source.sha256).ok_or(LoadError::NotFetched)
}

fn compiled(engine: &Engine, sha256: &str, bytes: &[u8]) -> Result<Component, LoadError> {
    static CACHE: OnceLock<Mutex<HashMap<String, Component>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(c) = cache.lock().ok().and_then(|m| m.get(sha256).cloned()) {
        return Ok(c);
    }
    let component = Component::from_binary(engine, bytes).map_err(|e| LoadError::Invalid(e.to_string()))?;
    if let Ok(mut m) = cache.lock() {
        if m.len() >= MAX_COMPILED {
            m.clear();
        }
        m.insert(sha256.to_owned(), component.clone());
    }
    Ok(component)
}

impl Module {
    /// A module that was not read: what validation of a document builds.
    #[must_use]
    pub(crate) fn unloaded(source: Source) -> Module {
        Module { source, loaded: None }
    }

    /// Reads, checks, compiles and links a module. It reads files and compiles: never call it on
    /// the request path.
    pub fn load(source: Source) -> Result<Module, LoadError> {
        let bytes = read(&source)?;
        let actual = digest(&bytes);
        if actual != source.sha256 {
            return Err(LoadError::Digest {
                expected: source.sha256,
                actual,
            });
        }
        let engine = engine()?;
        let component = compiled(engine, &source.sha256, &bytes)?;
        let mut linker = Linker::<Host>::new(engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(|e| LoadError::Invalid(e.to_string()))?;
        let pre = linker
            .instantiate_pre(&component)
            .map_err(|e| LoadError::Invalid(e.to_string()))?;
        let interface = component.get_export_index(None, INTERFACE);
        let exports = FUNCTIONS.map(|f| {
            interface
                .as_ref()
                .and_then(|i| component.get_export_index(Some(i), f))
        });
        Ok(Module {
            source,
            loaded: Some(Loaded { pre, exports }),
        })
    }

    /// The module's SHA-256, lowercase hex.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.source.sha256
    }

    #[must_use]
    pub fn source(&self) -> &Source {
        &self.source
    }

    /// Fails when a loaded module does not export the function of `hook`.
    pub fn check_export(&self, hook: Hook) -> Result<(), LoadError> {
        match &self.loaded {
            Some(l) if export_of(l, hook).is_none() => Err(LoadError::MissingExport(function(hook))),
            _ => Ok(()),
        }
    }

    /// Calls the hook's function with `payload` in a fresh instance; a trap, the memory cap or the
    /// time limit is a failure.
    pub async fn call(&self, hook: Hook, payload: String) -> Result<String, Failure> {
        let loaded = self
            .loaded
            .as_ref()
            .ok_or_else(|| Failure::new("the module is not loaded"))?;
        let export =
            export_of(loaded, hook).ok_or_else(|| Failure::new("the component does not export this hook"))?;
        let engine = engine().map_err(|e| Failure::new(e.to_string()))?;
        let mut store = Store::new(
            engine,
            Host {
                ctx: WasiCtx::builder().build(),
                table: ResourceTable::new(),
                limits: StoreLimitsBuilder::new()
                    .memory_size(self.source.max_memory)
                    .trap_on_grow_failure(true)
                    .build(),
            },
        );
        store.limiter(|h| &mut h.limits);
        store.set_epoch_deadline(self.source.ticks());
        store.epoch_deadline_trap();
        let instance = loaded
            .pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| Failure::new(format!("the component could not be instantiated: {e}")))?;
        let func = instance
            .get_typed_func::<(String,), (String,)>(&mut store, export)
            .map_err(|e| Failure::new(format!("the hook function has another signature: {e}")))?;
        let (answer,) = func
            .call_async(&mut store, (payload,))
            .await
            .map_err(|e| Failure::new(format!("the component trapped: {e}")))?;
        Ok(answer)
    }
}

#[cfg(test)]
mod tests;

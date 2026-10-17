use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use super::*;

/// What a test component's function does.
enum Body<'a> {
    Answer(&'a str),
    Loop,
    Grow,
    Trap,
}

fn escape(s: &str) -> String {
    s.bytes().map(|b| format!("\\{b:02x}")).collect()
}

fn func(name: &str, body: &Body<'_>, at: u32) -> (String, String) {
    let (data, code) = match body {
        Body::Answer(a) => {
            let text = at + 8;
            let len = u32::try_from(a.len()).unwrap();
            let record: Vec<u8> = text.to_le_bytes().into_iter().chain(len.to_le_bytes()).collect();
            (
                format!(
                    "(data (i32.const {at}) \"{}\") (data (i32.const {text}) \"{}\")",
                    record.iter().map(|b| format!("\\{b:02x}")).collect::<String>(),
                    escape(a)
                ),
                format!("i32.const {at}"),
            )
        }
        Body::Loop => (String::new(), "(loop $l (br $l)) i32.const 0".to_owned()),
        Body::Grow => (
            String::new(),
            "(drop (memory.grow (i32.const 4000))) i32.const 0".to_owned(),
        ),
        Body::Trap => (String::new(), "unreachable".to_owned()),
    };
    (
        data,
        format!("(func (export \"{name}\") (param i32 i32) (result i32) {code})"),
    )
}

/// A component exporting `functions` of the handler interface.
fn component(functions: &[(&str, Body<'_>)]) -> Vec<u8> {
    let mut data = String::new();
    let mut funcs = String::new();
    let mut lifts = String::new();
    let mut exports = String::new();
    for (i, (name, body)) in functions.iter().enumerate() {
        let (d, f) = func(name, body, 16 + 1024 * u32::try_from(i).unwrap());
        data.push_str(&d);
        funcs.push_str(&f);
        lifts.push_str(&format!(
            "(func $f{i} (param \"payload\" string) (result string) (canon lift (core func $i \"{name}\") (memory (core memory $i \"memory\")) (realloc (core func $i \"realloc\"))))"
        ));
        exports.push_str(&format!("(export \"{name}\" (func $f{i}))"));
    }
    let text = format!(
        "(component
           (core module $m
             (memory (export \"memory\") 1)
             (global $bump (mut i32) (i32.const 32768))
             (func (export \"realloc\") (param i32 i32 i32 i32) (result i32)
               (local $p i32)
               (local.set $p (global.get $bump))
               (global.set $bump (i32.add (global.get $bump) (local.get 3)))
               (local.get $p))
             {data}
             {funcs})
           (core instance $i (instantiate $m))
           {lifts}
           (instance $handler {exports})
           (export \"antirion:extension/handler@1.0.0\" (instance $handler)))"
    );
    wat::parse_str(&text).unwrap()
}

fn dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("antirion-wasm-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn module_of(name: &str, bytes: &[u8], max_duration: Duration) -> Result<Module, LoadError> {
    let path = dir().join(name);
    std::fs::write(&path, bytes).unwrap();
    Module::load(Source {
        module: format!("file://{}", path.display()),
        sha256: digest(bytes),
        max_memory: 16 << 20,
        max_duration,
    })
}

/// Advances the epoch clock while a test's calls run.
struct Ticker(Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>);

impl Ticker {
    #[allow(clippy::disallowed_methods)]
    fn start() -> Ticker {
        let stop = Arc::new(AtomicBool::new(false));
        let s = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                tick();
                std::thread::sleep(TICK);
            }
        });
        Ticker(stop, Some(handle))
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
        if let Some(h) = self.1.take() {
            let _ = h.join();
        }
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(f)
}

#[test]
fn a_component_answers_its_hooks() {
    let bytes = component(&[
        ("request", Body::Answer(r#"{"action":"continue"}"#)),
        (
            "stream-chunk",
            Body::Answer(r#"{"action":"deny","status":403,"message":"no"}"#),
        ),
    ]);
    let m = module_of("answers.wasm", &bytes, Duration::from_millis(50)).unwrap();
    assert!(m.check_export(Hook::Request).is_ok());
    assert!(matches!(
        m.check_export(Hook::Attempt),
        Err(LoadError::MissingExport("attempt"))
    ));
    let _t = Ticker::start();
    let out = block_on(m.call(Hook::Request, "{}".into())).unwrap();
    assert_eq!(out, r#"{"action":"continue"}"#);
    let out = block_on(m.call(Hook::StreamChunk, "{\"event\":{}}".into())).unwrap();
    assert!(out.contains("deny"));
}

#[test]
fn a_component_that_loops_is_stopped_at_its_time_limit() {
    let bytes = component(&[("request", Body::Loop)]);
    let m = module_of("loops.wasm", &bytes, Duration::from_millis(5)).unwrap();
    let _t = Ticker::start();
    let started = Instant::now();
    let r = block_on(m.call(Hook::Request, "{}".into()));
    assert!(r.is_err());
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn memory_beyond_the_cap_and_traps_are_failures() {
    let bytes = component(&[("request", Body::Grow), ("response", Body::Trap)]);
    let m = module_of("grows.wasm", &bytes, Duration::from_millis(50)).unwrap();
    let _t = Ticker::start();
    assert!(block_on(m.call(Hook::Request, "{}".into())).is_err());
    assert!(block_on(m.call(Hook::Response, "{}".into())).is_err());
}

#[test]
fn a_module_must_have_its_digest() {
    let bytes = component(&[("request", Body::Answer("{}"))]);
    let path = dir().join("digest.wasm");
    std::fs::write(&path, &bytes).unwrap();
    let source = Source {
        module: format!("file://{}", path.display()),
        sha256: "0".repeat(64),
        max_memory: 1 << 20,
        max_duration: Duration::from_millis(5),
    };
    let e = Module::load(source).unwrap_err();
    assert_eq!(e.field(), "sha256");
    assert!(matches!(e, LoadError::Digest { .. }));
    let missing = Source {
        module: "file:///nonexistent/antirion.wasm".into(),
        sha256: "0".repeat(64),
        max_memory: 1 << 20,
        max_duration: Duration::from_millis(5),
    };
    assert!(matches!(Module::load(missing), Err(LoadError::Unreadable(_))));
    let not_component = b"\0asm\x01\0\0\0".to_vec();
    assert!(matches!(
        module_of("core.wasm", &not_component, Duration::from_millis(5)),
        Err(LoadError::Invalid(_))
    ));
}

#[test]
fn ticks_round_the_limit_up() {
    let s = |ms| Source {
        module: String::new(),
        sha256: String::new(),
        max_memory: 0,
        max_duration: Duration::from_micros(ms),
    };
    assert_eq!(s(5_000).ticks(), 6);
    assert_eq!(s(4_500).ticks(), 6);
    assert_eq!(s(1).ticks(), 2);
}

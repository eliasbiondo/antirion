//! Chooses the directory the console is embedded from: `ANTIRION_CONSOLE_DIR` when set, else the
//! frontend's static export in `console/out` when it holds an `index.html`, else the placeholder page,
//! so a Rust-only build never needs the frontend toolchain.

use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let export = manifest.join("../../console/out");
    let chosen = std::env::var_os("ANTIRION_CONSOLE_DIR")
        .map(PathBuf::from)
        .filter(|d| !d.as_os_str().is_empty())
        .or_else(|| export.join("index.html").is_file().then(|| export.clone()));
    let (dir, built) = match chosen {
        Some(d) => (d, true),
        None => (manifest.join("placeholder"), false),
    };
    let dir = dir.canonicalize().unwrap_or(dir);
    println!("cargo:rustc-env=ANTIRION_CONSOLE_ASSETS={}", dir.display());
    println!("cargo:rustc-env=ANTIRION_CONSOLE_BUILT={}", u8::from(built));
    println!("cargo:rerun-if-env-changed=ANTIRION_CONSOLE_DIR");
    watch(&dir);
    if export.is_dir() {
        watch(&export);
    }
}

fn watch(dir: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
}

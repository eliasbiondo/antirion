//! Chooses the directory the console is embedded from: `ANTIRION_CONSOLE_DIR` when set, else the
//! frontend's static export in `console/out` when it holds an `index.html`, else the placeholder page,
//! so a Rust-only build never needs the frontend toolchain.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MIN_SIZE: u64 = 1024;
const TEXT: &[&str] = &[
    "html",
    "js",
    "mjs",
    "css",
    "json",
    "map",
    "txt",
    "svg",
    "xml",
    "webmanifest",
];

fn main() -> io::Result<()> {
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
    let gzip = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default()).join("gzip");
    if gzip.exists() {
        fs::remove_dir_all(&gzip)?;
    }
    fs::create_dir_all(&gzip)?;
    compress_tree(&dir, &dir, &gzip)?;
    println!("cargo:rustc-env=ANTIRION_CONSOLE_ASSETS={}", dir.display());
    println!("cargo:rustc-env=ANTIRION_CONSOLE_GZIP={}", gzip.display());
    println!("cargo:rustc-env=ANTIRION_CONSOLE_BUILT={}", u8::from(built));
    println!("cargo:rerun-if-env-changed=ANTIRION_CONSOLE_DIR");
    watch(&dir);
    if export.is_dir() {
        watch(&export);
    }
    Ok(())
}

fn compress_tree(root: &Path, dir: &Path, out: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            compress_tree(root, &path, out)?;
            continue;
        }
        let text = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| TEXT.contains(&e.to_ascii_lowercase().as_str()));
        if !text || fs::metadata(&path)?.len() < MIN_SIZE {
            continue;
        }
        let raw = fs::read(&path)?;
        let packed = gzip(&raw);
        if packed.len() > raw.len() - raw.len() / 8 {
            continue;
        }
        let target = out.join(path.strip_prefix(root).unwrap_or(&path));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target, packed)?;
    }
    Ok(())
}

/// A gzip member (RFC 1952) with no name and no timestamp, so the same export gives the same bytes.
fn gzip(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 2, 0xff];
    out.extend(miniz_oxide::deflate::compress_to_vec(raw, 9));
    out.extend(crc32fast::hash(raw).to_le_bytes());
    out.extend((raw.len() as u32).to_le_bytes());
    out
}

fn watch(dir: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
}

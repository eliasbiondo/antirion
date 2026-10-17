//! Repository automation: `cargo xtask <command>`.
//!
//! - `policy`: forbidden patterns (open-work markers) in the repository's own text (`ci/policy.txt`).
//! - `layers`: every crate depends only on crates of a lower layer.
//! - `lints`: every crate inherits the workspace lints; request-path crates deny panics.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, bail};
use cargo_metadata::{DependencyKind, MetadataCommand};
use regex::Regex;

/// The repository root: the parent of the manifest directory `cargo run` passes at run time, so no
/// build-time path is compiled into the binary.
fn root() -> PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .and_then(|dir| dir.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let result = match cmd.as_str() {
        "policy" => policy(),
        "layers" => layers(),
        "lints" => lints(),
        _ => Err(anyhow::anyhow!("usage: cargo xtask <policy|layers|lints>")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask {cmd}: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Files the policy does not read: the lockfiles, the vendored contract and schema, and the policy
/// itself.
const UNCHECKED: &[&str] = &[
    "Cargo.lock",
    "bench/Cargo.lock",
    "console/pnpm-lock.yaml",
    "openapi/admin-api.yaml",
    "schema/antirion-config.schema.json",
    "ci/policy.txt",
];

/// Every file git tracks or would track (not ignored), but the unchecked ones.
fn checked_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let out = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .output()
        .context("cannot run git ls-files")?;
    if !out.status.success() {
        bail!("git ls-files failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty() && !UNCHECKED.contains(l))
        .map(|l| root.join(l))
        .collect())
}

/// The patterns of `ci/policy.txt`: one `regex:` line each.
fn patterns(text: &str) -> anyhow::Result<Vec<Regex>> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|line| {
            let r = line
                .strip_prefix("regex:")
                .with_context(|| format!("ci/policy.txt: cannot read the line {line:?}"))?;
            Regex::new(r.trim()).with_context(|| format!("bad pattern {r}"))
        })
        .collect()
}

fn policy() -> anyhow::Result<()> {
    let root = root();
    let text = std::fs::read_to_string(root.join("ci/policy.txt")).context("cannot read ci/policy.txt")?;
    let patterns = patterns(&text)?;
    let mut findings = Vec::new();
    for file in checked_files(&root)? {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let rel = file.strip_prefix(&root).unwrap_or(&file).display().to_string();
        for (n, line) in text.lines().enumerate() {
            for p in &patterns {
                if let Some(m) = p.find(line) {
                    findings.push(format!(
                        "{rel}:{}: matches /{}/: {:?}",
                        n + 1,
                        p.as_str(),
                        m.as_str()
                    ));
                }
            }
        }
    }
    if findings.is_empty() {
        Ok(())
    } else {
        bail!("{} finding(s):\n{}", findings.len(), findings.join("\n"))
    }
}

/// The layer of every crate; a crate may depend only on lower layers (and the error crate on the
/// core crate, the one same-layer edge).
const LAYERS: &[(&str, u8)] = &[
    ("antirion-core", 0),
    ("antirion-error", 0),
    ("antirion-json", 0),
    ("antirion-console", 0),
    ("antirion-wire", 1),
    ("antirion-catalog", 2),
    ("antirion-config", 1),
    ("antirion-mail", 1),
    ("antirion-dialects", 2),
    ("antirion-egress", 2),
    ("antirion-identity", 2),
    ("antirion-store", 2),
    ("antirion-obs", 2),
    ("antirion-providers", 3),
    ("antirion-router", 3),
    ("antirion-resilience", 3),
    ("antirion-limits", 3),
    ("antirion-cache", 3),
    ("antirion-guardrails", 3),
    ("antirion-ext", 3),
    ("antirion-mcp", 3),
    ("antirion-runtime", 4),
    ("antirion-pipeline", 5),
    ("antirion-admin", 5),
    ("antirion-server", 6),
    ("antirion", 7),
    ("antirion-testkit", 8),
    ("antirion-redis", 2),
];

/// Crates on the request path, which deny unwrap, expect, panic, indexing and unreachable.
const REQUEST_PATH: &[&str] = &[
    "antirion-json",
    "antirion-wire",
    "antirion-dialects",
    "antirion-egress",
    "antirion-identity",
    "antirion-router",
    "antirion-resilience",
    "antirion-limits",
    "antirion-cache",
    "antirion-guardrails",
    "antirion-ext",
    "antirion-mcp",
    "antirion-pipeline",
    "antirion-redis",
];

const PANIC_FREE: &str = "#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing, clippy::unreachable)]";

fn layers() -> anyhow::Result<()> {
    let metadata = MetadataCommand::new()
        .manifest_path(root().join("Cargo.toml"))
        .no_deps()
        .exec()?;
    let layer: BTreeMap<&str, u8> = LAYERS.iter().copied().collect();
    let mut problems = Vec::new();
    for pkg in &metadata.packages {
        if pkg.name.as_str() == "xtask" {
            continue;
        }
        let Some(own) = layer.get(pkg.name.as_str()) else {
            problems.push(format!("{} has no layer", pkg.name));
            continue;
        };
        for dep in pkg
            .dependencies
            .iter()
            .filter(|d| d.kind == DependencyKind::Normal)
        {
            let Some(theirs) = layer.get(dep.name.as_str()) else {
                continue;
            };
            if dep.name == "antirion-testkit" {
                problems.push(format!(
                    "{} depends on antirion-testkit outside dev-dependencies",
                    pkg.name
                ));
                continue;
            }
            let same_layer_exception = pkg.name.as_str() == "antirion-error" && dep.name == "antirion-core";
            if theirs >= own && !same_layer_exception {
                problems.push(format!(
                    "{} (layer {own}) depends on {} (layer {theirs})",
                    pkg.name, dep.name
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        bail!("{}", problems.join("\n"))
    }
}

fn lints() -> anyhow::Result<()> {
    let metadata = MetadataCommand::new()
        .manifest_path(root().join("Cargo.toml"))
        .no_deps()
        .exec()?;
    let mut problems = Vec::new();
    for pkg in &metadata.packages {
        let manifest = std::fs::read_to_string(pkg.manifest_path.as_std_path())?;
        let table: toml_lite::Manifest = toml_lite::Manifest::parse(&manifest);
        if !table.inherits_workspace_lints {
            problems.push(format!("{} does not inherit the workspace lints", pkg.name));
        }
        if REQUEST_PATH.contains(&pkg.name.as_str()) {
            let lib = pkg.manifest_path.as_std_path().with_file_name("src/lib.rs");
            let source = std::fs::read_to_string(&lib).unwrap_or_default();
            let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
            let wanted: String = PANIC_FREE.chars().filter(|c| !c.is_whitespace()).collect();
            if !compact.replace(",)]", ")]").contains(&wanted) {
                problems.push(format!(
                    "{} lacks the panic-free lint block in src/lib.rs",
                    pkg.name
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        bail!("{}", problems.join("\n"))
    }
}

/// Just enough manifest reading for the lint check: whether `[lints]` has `workspace = true`.
mod toml_lite {
    pub(crate) struct Manifest {
        pub(crate) inherits_workspace_lints: bool,
    }

    impl Manifest {
        pub(crate) fn parse(text: &str) -> Manifest {
            let mut in_lints = false;
            let mut inherits = false;
            for line in text.lines().map(str::trim) {
                if line.starts_with('[') {
                    in_lints = line == "[lints]";
                } else if in_lints && line.replace(' ', "") == "workspace=true" {
                    inherits = true;
                }
            }
            Manifest {
                inherits_workspace_lints: inherits,
            }
        }
    }
}

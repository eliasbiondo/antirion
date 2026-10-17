//! Records the commit the binary is built from, for `antirion version` and the status endpoint, and
//! the compiler version, for `antirion_build_info`.

use std::process::Command;

fn main() {
    let commit = std::env::var("ANTIRION_BUILD_COMMIT")
        .ok()
        .filter(|c| !c.is_empty())
        .or_else(git_commit)
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=ANTIRION_COMMIT={commit}");
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().nth(1).map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=ANTIRION_RUSTC={version}");
    println!("cargo:rerun-if-env-changed=ANTIRION_BUILD_COMMIT");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
}

/// The commit of the working copy, when it is a git checkout.
fn git_commit() -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

//! The `antirion` binary.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::Parser;

#[cfg(all(target_os = "linux", target_env = "gnu", not(feature = "mimalloc")))]
#[global_allocator]
static ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(not(all(target_os = "linux", target_env = "gnu", not(feature = "mimalloc"))))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    antirion::run(antirion::Cli::parse())
}

//! `antirion-bench-mock`: the benchmark mock upstream as its own process.

use std::net::SocketAddr;
use std::sync::Arc;

use antirion_bench_mock::{Profile, Stats, serve};
use anyhow::Context;
use clap::Parser;
use tokio_util::sync::CancellationToken;

/// The benchmark mock upstream.
#[derive(Debug, Parser)]
#[command(name = "antirion-bench-mock")]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:19401")]
    listen: SocketAddr,
    /// Latency profile for paths without a `/<profile>/` prefix.
    #[arg(long, default_value = "L0")]
    profile: Profile,
    /// Runtime worker threads (default: one per CPU).
    #[arg(long)]
    workers: Option<usize>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut rt = tokio::runtime::Builder::new_multi_thread();
    rt.enable_all();
    if let Some(n) = args.workers {
        rt.worker_threads(n);
    }
    rt.build()?.block_on(async move {
        let listener = tokio::net::TcpListener::bind(args.listen)
            .await
            .with_context(|| format!("cannot listen on {}", args.listen))?;
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();
        let server = serve(listener, args.profile, Arc::new(Stats::default()), shutdown);
        tokio::select! {
            r = server => r.context("mock server failed")?,
            () = signal() => stop.cancel(),
        }
        Ok(())
    })
}

async fn signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) else {
            let _ = ctrl_c.await;
            return;
        };
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}

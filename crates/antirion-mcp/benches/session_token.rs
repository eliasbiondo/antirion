//! Decoding and verifying an MCP session token, as every request of a multiplexed session does.

use std::hint::black_box;

use antirion_mcp::SessionTokenSample;
use criterion::{Criterion, criterion_group, criterion_main};

fn session_token(c: &mut Criterion) {
    let Some(sample) = SessionTokenSample::new(2) else {
        return;
    };
    let token = sample.token().to_owned();
    let mut group = c.benchmark_group("session_token");
    group.bench_function("open", |b| b.iter(|| black_box(sample.open(black_box(&token)))));
    group.finish();
}

criterion_group!(benches, session_token);
criterion_main!(benches);

//! The self-tests that run before every suite: the committed corpus, the mock's own latency at the
//! topology's rate, the generator's schedule, the mock's latency profiles and protocols, and a smoke
//! test of every scenario through the gateway.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use antirion_bench_loadgen::{
    ClosedLoop, Nodes, OpenLoop, Protocol, RunReport, Source, closed_loop, open_loop,
};
use antirion_bench_mock::Profile;
use antirion_bench_report::model::{CellDef, write_json};
use serde::Serialize;

use crate::cells::{bring_down, bring_up};
use crate::ctx::Ctx;
use crate::gateway;
use crate::sampler;
use crate::source::{self, ScenarioSource, SourceCfg};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Check {
    pub(crate) name: String,
    pub(crate) passed: bool,
    pub(crate) detail: String,
}

fn check(name: &str, passed: bool, detail: String) -> Check {
    eprintln!(
        "selftest {name}: {} — {detail}",
        if passed { "ok" } else { "FAILED" }
    );
    Check {
        name: name.to_owned(),
        passed,
        detail,
    }
}

async fn quick(
    nodes: &[SocketAddr],
    source: Arc<dyn Source>,
    rate: f64,
    secs: f64,
    protocol: Protocol,
    hold: bool,
) -> anyhow::Result<RunReport> {
    let n = Arc::new(Nodes::connect(nodes, protocol).await?);
    let cfg = OpenLoop {
        rate,
        warmup: Duration::from_millis(500),
        measure: Duration::from_secs_f64(secs),
        timeout: Duration::from_secs(30),
        hold,
    };
    let r = open_loop(
        cfg,
        source,
        Arc::clone(&n),
        Instant::now() + Duration::from_millis(100),
    )
    .await;
    if let Ok(n) = Arc::try_unwrap(n) {
        n.close().await;
    }
    Ok(r)
}

fn direct(
    ctx: &Ctx,
    scenario: &str,
    profile: Profile,
    protocol_h2: bool,
    session: Option<&str>,
) -> anyhow::Result<Arc<ScenarioSource>> {
    let l = source::load(&ctx.paths, &ctx.scenarios, scenario)?;
    let cfg = SourceCfg::new(vec![ctx.mock_addr], protocol_h2, 0.0);
    Ok(Arc::new(ScenarioSource::direct(&l, &cfg, profile, session)?))
}

fn ms(us: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let v = us as f64 / 1000.0;
    v
}

const H1: Protocol = Protocol::Http1 {
    max_connections: 1024,
};

async fn mock_checks(ctx: &Ctx, out: &mut Vec<Check>) -> anyhow::Result<()> {
    let slack = if ctx.debug_build { 20.0 } else { 1.0 };
    let st = &ctx.topo.selftest;
    let r = quick(
        &[ctx.mock_addr],
        direct(ctx, "S1", Profile::L0, false, None)?,
        st.mock_rate,
        st.duration_s,
        Protocol::Http1 {
            max_connections: ctx.topo.generator.max_connections,
        },
        false,
    )
    .await?;
    #[allow(clippy::cast_precision_loss)]
    let p99 = r.hist.latency.value_at_quantile(0.99) as f64;
    let bound = st.mock_p99_us * slack;
    out.push(check(
        "mock-latency",
        p99 <= bound && r.counts.ok == r.counts.offered && r.counts.offered > 0,
        format!(
            "L0 at {} RPS: p99 {p99:.0} µs (bound {bound:.0} µs{}), {} of {} ok",
            st.mock_rate,
            if ctx.debug_build {
                ", relaxed for a debug build"
            } else {
                ""
            },
            r.counts.ok,
            r.counts.offered
        ),
    ));
    #[allow(clippy::cast_precision_loss)]
    let late = r.counts.late as f64 / r.counts.offered.max(1) as f64;
    let cpu = r.generator_cpu_s / (st.duration_s * sampler::cpus()) * 100.0;
    out.push(check(
        "generator-schedule",
        late <= 0.001 * slack && cpu <= 60.0,
        format!(
            "{:.3} % sent over 1 ms late, lag p99 {} µs, generator CPU {cpu:.1} %",
            late * 100.0,
            r.hist.lag.value_at_quantile(0.99)
        ),
    ));
    let r = quick(
        &[ctx.mock_addr],
        direct(ctx, "S1", Profile::L60, false, None)?,
        20.0,
        2.0,
        H1,
        false,
    )
    .await?;
    let p50 = ms(r.hist.latency.value_at_quantile(0.5));
    out.push(check(
        "profile-L60",
        (60.0..65.0 + slack * 2.0).contains(&p50) && r.counts.ok == r.counts.offered,
        format!("p50 {p50:.1} ms"),
    ));
    let r = quick(
        &[ctx.mock_addr],
        direct(ctx, "S3", Profile::Ls, false, None)?,
        10.0,
        2.0,
        H1,
        false,
    )
    .await?;
    let (ttft, total) = (
        ms(r.hist.ttft.value_at_quantile(0.5)),
        ms(r.hist.latency.value_at_quantile(0.5)),
    );
    out.push(check(
        "profile-LS",
        (215.0..235.0 + slack).contains(&ttft) && (995.0..1030.0 + slack * 2.0).contains(&total),
        format!(
            "TTFT p50 {ttft:.1} ms, stream p50 {total:.1} ms, {} gaps",
            r.hist.gap.len()
        ),
    ));
    let r = quick(
        &[ctx.mock_addr],
        direct(ctx, "S3", Profile::LsIdle, false, None)?,
        10.0,
        1.0,
        H1,
        true,
    )
    .await?;
    out.push(check(
        "profile-LS-idle",
        r.counts.held == r.counts.offered && r.counts.offered > 0,
        format!(
            "{} of {} streams still open at the end",
            r.counts.held, r.counts.offered
        ),
    ));
    let h2 = Protocol::H2c {
        connections: 2,
        streams: 64,
    };
    let r = quick(
        &[ctx.mock_addr],
        direct(ctx, "S5", Profile::L0, true, None)?,
        50.0,
        1.0,
        h2,
        false,
    )
    .await?;
    out.push(check(
        "h2c",
        r.counts.ok == r.counts.offered && !r.hist.ttft.is_empty(),
        format!(
            "{} of {} Messages streams over h2c",
            r.counts.ok, r.counts.offered
        ),
    ));
    let n = Arc::new(Nodes::connect(&[ctx.mock_addr], Protocol::Http1 { max_connections: 4 }).await?);
    let cfg = ClosedLoop {
        concurrency: 4,
        warmup: Duration::from_millis(200),
        measure: Duration::from_secs(1),
        timeout: Duration::from_secs(10),
    };
    let r = closed_loop(
        cfg,
        direct(ctx, "S6", Profile::L0, false, None)?,
        Arc::clone(&n),
        Instant::now(),
    )
    .await;
    if let Ok(n) = Arc::try_unwrap(n) {
        n.close().await;
    }
    out.push(check(
        "closed-loop",
        r.counts.ok > 0 && r.connections == 4,
        format!(
            "{} embeddings at concurrency 4 on {} connections",
            r.counts.ok, r.connections
        ),
    ));
    let sid = gateway::mcp_session(ctx.mock_addr, "/L0/mcp", None).await?;
    let r = quick(
        &[ctx.mock_addr],
        direct(ctx, "S7-legacy", Profile::L0, false, Some(&sid))?,
        20.0,
        1.0,
        H1,
        false,
    )
    .await?;
    out.push(check(
        "mock-mcp",
        r.counts.ok == r.counts.offered && r.counts.offered > 0,
        format!(
            "{} of {} tools/call in a legacy session",
            r.counts.ok, r.counts.offered
        ),
    ));
    Ok(())
}

/// Every scenario through the gateway with P-std: the expected statuses, stream timing, the
/// overhead header.
async fn gateway_checks(ctx: &Ctx, out: &mut Vec<Check>) -> anyhow::Result<()> {
    let base: CellDef = serde_json::from_value(serde_json::json!({
        "cell": "selftest", "kind": "open", "profile": "P-std", "informational": true,
    }))?;
    let started = Instant::now();
    let u = bring_up(ctx, &base, None, "selftest").await?;
    out.push(check(
        "gateway-start",
        true,
        format!("P-std ready in {:.1} s", started.elapsed().as_secs_f64()),
    ));
    let result = async {
        for name in [
            "S1",
            "S2",
            "S3",
            "S4",
            "S5",
            "S6",
            "S7-legacy",
            "S7-modern",
            "S8",
            "S8-403",
            "S8-404",
            "S8-429",
            "M1",
        ] {
            let l = source::load(&ctx.paths, &ctx.scenarios, name)?;
            let gw = u.gw.as_ref();
            let mut keys = match gw {
                Some(g) => source::keys_for(&l, &g.rendered.used_keys, &g.rendered.named)?,
                None => u.keys.clone(),
            };
            let mut sessions = Vec::new();
            if l.session.as_deref() == Some("mcp_legacy")
                && let Some(g) = gw
            {
                keys.truncate(4);
                sessions = gateway::mcp_sessions(g, &keys).await?;
            }
            let cfg = SourceCfg::new(u.nodes.clone(), false, 0.0);
            let src = Arc::new(ScenarioSource::gateway(&l, &cfg, &keys, &sessions, &u.models)?);
            let r = quick(&u.nodes, src, 20.0, 0.5, H1, false).await?;
            let mut ok = r.counts.ok == r.counts.offered && r.counts.offered > 0;
            let mut detail = format!("{} of {} as expected", r.counts.ok, r.counts.offered);
            if !r.counts.non_2xx.is_empty() {
                detail.push_str(&format!(", other statuses {:?}", r.counts.non_2xx));
            }
            if l.stream && name != "M1" {
                ok &= !r.hist.ttft.is_empty();
                detail.push_str(&format!(
                    ", TTFT p50 {:.2} ms",
                    ms(r.hist.ttft.value_at_quantile(0.5))
                ));
            }
            if name == "S1" {
                ok &= !r.hist.overhead_header.is_empty();
                detail.push_str(&format!(
                    ", overhead header p50 {} µs",
                    r.hist.overhead_header.value_at_quantile(0.5)
                ));
            }
            out.push(check(&format!("gateway-{name}"), ok, detail));
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    bring_down(ctx, u).await;
    result
}

/// Runs every self-test, writes `selftest.json`; whether all passed.
pub(crate) async fn run(ctx: &Ctx, with_gateway: bool) -> anyhow::Result<bool> {
    let mut checks = Vec::new();
    match antirion_bench_corpus::check(&ctx.paths.corpus()) {
        Ok(()) => checks.push(check(
            "corpus",
            true,
            "committed corpus matches its seed".to_owned(),
        )),
        Err(e) => checks.push(check("corpus", false, e.to_string())),
    }
    if let Err(e) = mock_checks(ctx, &mut checks).await {
        checks.push(check("mock", false, format!("{e:#}")));
    }
    if with_gateway && let Err(e) = gateway_checks(ctx, &mut checks).await {
        checks.push(check("gateway", false, format!("{e:#}")));
    }
    let passed = checks.iter().all(|c| c.passed);
    write_json(
        &ctx.out.join("selftest.json"),
        &serde_json::json!({
            "passed": passed,
            "topology": ctx.topo.name,
            "debug_build": ctx.debug_build,
            "checks": checks,
        }),
    )?;
    Ok(passed)
}

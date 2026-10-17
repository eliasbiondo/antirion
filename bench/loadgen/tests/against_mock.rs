use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use antirion_bench_loadgen::{
    ClosedLoop, Nodes, OpenLoop, Prepared, Protocol, Source, closed_loop, open_loop,
};
use antirion_bench_mock::{Profile, Stats, serve};
use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use tokio_util::sync::CancellationToken;

struct Fixed {
    path: &'static str,
    body: &'static str,
    disconnect: bool,
}

impl Source for Fixed {
    fn prepare(&self, _index: u64) -> Prepared {
        let request = Request::post(self.path)
            .header("host", "mock")
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from_static(self.body.as_bytes())))
            .unwrap();
        Prepared {
            node: 0,
            request,
            expect: 200,
            disconnect: self.disconnect,
        }
    }
}

#[allow(clippy::disallowed_methods)]
async fn mock() -> (
    SocketAddr,
    CancellationToken,
    Arc<Stats>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = CancellationToken::new();
    let stats = Arc::new(Stats::default());
    let handle = tokio::task::spawn({
        let (stop, stats) = (stop.clone(), Arc::clone(&stats));
        async move {
            serve(listener, Profile::L0, stats, stop).await.unwrap();
        }
    });
    (addr, stop, stats, handle)
}

fn open(rate: f64) -> OpenLoop {
    OpenLoop {
        rate,
        warmup: Duration::from_millis(200),
        measure: Duration::from_millis(800),
        timeout: Duration::from_secs(5),
        hold: false,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::disallowed_methods)]
async fn open_loop_counts_every_request_of_the_window() {
    let (addr, stop, stats, server) = mock().await;
    let nodes = Arc::new(
        Nodes::connect(&[addr], Protocol::Http1 { max_connections: 64 })
            .await
            .unwrap(),
    );
    let src = Arc::new(Fixed {
        path: "/v1/chat/completions",
        body: r#"{"model":"m","stream":false}"#,
        disconnect: false,
    });
    let r = open_loop(
        open(200.0),
        src,
        Arc::clone(&nodes),
        Instant::now() + Duration::from_millis(20),
    )
    .await;
    assert_eq!(r.counts.offered, 160);
    assert_eq!(r.counts.ok, 160, "{:?}", r.counts);
    assert_eq!(r.hist.latency.len(), 160);
    assert!(stats.upstream() >= 200);
    assert!(r.hist.ttft.is_empty());
    Arc::into_inner(nodes).unwrap().close().await;
    stop.cancel();
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::disallowed_methods)]
async fn streams_record_ttft_and_gaps_and_profiles_apply() {
    let (addr, stop, _stats, server) = mock().await;
    let nodes = Arc::new(
        Nodes::connect(
            &[addr],
            Protocol::H2c {
                connections: 2,
                streams: 16,
            },
        )
        .await
        .unwrap(),
    );
    let src = Arc::new(Fixed {
        path: "/L60/v1/chat/completions",
        body: r#"{"model":"m","stream":true}"#,
        disconnect: false,
    });
    let r = open_loop(
        open(50.0),
        src,
        Arc::clone(&nodes),
        Instant::now() + Duration::from_millis(20),
    )
    .await;
    assert_eq!(r.counts.ok, 40, "{:?}", r.counts);
    let ttft_ms = r.hist.ttft.value_at_quantile(0.5) / 1000;
    assert!((64..120).contains(&ttft_ms), "ttft {ttft_ms} ms");
    let total_ms = r.hist.latency.value_at_quantile(0.5) / 1000;
    assert!((260..400).contains(&total_ms), "total {total_ms} ms");
    assert_eq!(r.hist.gap.len(), 40 * 42);
    Arc::into_inner(nodes).unwrap().close().await;

    let nodes = Arc::new(
        Nodes::connect(&[addr], Protocol::Http1 { max_connections: 8 })
            .await
            .unwrap(),
    );
    let src = Arc::new(Fixed {
        path: "/LS/v1/chat/completions",
        body: r#"{"model":"m","stream":true}"#,
        disconnect: true,
    });
    let r = open_loop(open(20.0), src, Arc::clone(&nodes), Instant::now()).await;
    assert_eq!(r.counts.disconnects, 16, "{:?}", r.counts);
    Arc::into_inner(nodes).unwrap().close().await;
    stop.cancel();
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::disallowed_methods)]
async fn closed_loop_keeps_its_concurrency() {
    let (addr, stop, _stats, server) = mock().await;
    let nodes = Arc::new(
        Nodes::connect(&[addr], Protocol::Http1 { max_connections: 4 })
            .await
            .unwrap(),
    );
    let src = Arc::new(Fixed {
        path: "/L60/v1/embeddings",
        body: "{}",
        disconnect: false,
    });
    let cfg = ClosedLoop {
        concurrency: 4,
        warmup: Duration::from_millis(100),
        measure: Duration::from_millis(600),
        timeout: Duration::from_secs(5),
    };
    let r = closed_loop(cfg, src, Arc::clone(&nodes), Instant::now()).await;
    assert!((30..=48).contains(&r.counts.ok), "{:?}", r.counts);
    assert_eq!(r.connections, 4);
    Arc::into_inner(nodes).unwrap().close().await;
    stop.cancel();
    server.await.unwrap();
}

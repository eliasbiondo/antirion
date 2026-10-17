//! A cluster node whose store does not answer while it starts. Run when
//! `ANTIRION_TEST_POSTGRES_URL` names a PostgreSQL server the tests may create databases on:
//!
//! ```text
//! ANTIRION_TEST_POSTGRES_URL=postgres://antirion:antirion@127.0.0.1:55432/antirion cargo test -p antirion --test cluster_start
//! ```

use std::net::SocketAddr;
use std::time::Duration;

use antirion::startup::{Node, ServeOptions, start};
use antirion_store::Store;
use antirion_store::pg::{Migrate, PgSettings};
use antirion_testkit::ScratchDir;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_postgres::NoTls;

const TOKEN: &str = "cluster-start-bootstrap-token-0123456789abcdef";
const ENCRYPTION_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

/// A fresh database on the test server: its URL, and a client of it.
async fn fresh_database(drivers: &mut JoinSet<()>) -> Option<String> {
    let base = std::env::var("ANTIRION_TEST_POSTGRES_URL").ok()?;
    let name = format!("start_{:x}", antirion_core::ResourceUid::random().0);
    let (client, conn) = tokio_postgres::connect(&base, NoTls).await.ok()?;
    drivers.spawn(async move {
        let _ = conn.await;
    });
    client
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .await
        .ok()?;
    let (prefix, _) = base.rsplit_once('/')?;
    Some(format!("{prefix}/{name}"))
}

/// Migrates the schema the way a node would, so a test can hold its tables.
async fn migrate(url: &str) {
    let store = Store::open_postgres(&PgSettings {
        url: url.to_owned(),
        pool_size: 1,
        connect_timeout: Duration::from_secs(5),
        statement_timeout: Duration::from_secs(5),
        migrate: Migrate::Auto,
        migrate_timeout: Duration::from_secs(30),
        binary_version: env!("CARGO_PKG_VERSION").to_owned(),
        node_id: "migrator".to_owned(),
    })
    .await
    .unwrap();
    store.close();
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A cluster node's startup file: the store at `store_url`, a counter store nobody listens on,
/// and a model with a key in its dynamic part.
fn config(dir: &ScratchDir, store_url: &str, data: u16, admin: u16) -> std::path::PathBuf {
    let doc = json!({
        "version": 1,
        "server": {"listen": format!("127.0.0.1:{data}")},
        "admin": {"listen": format!("127.0.0.1:{admin}")},
        "cluster": {
            "mode": "cluster",
            "node_id": "start-test",
            "probe_interval": "100ms",
            "counters": {"backend": "redis", "redis": {"url": "redis://127.0.0.1:1/0"}}
        },
        "store": {
            "backend": "postgres",
            "embedded": {"path": dir.path().join("data").to_string_lossy()},
            "postgres": {
                "url": store_url,
                "pool_size": 2,
                "connect_timeout": "500ms",
                "statement_timeout": "500ms"
            }
        },
        "security": {"secrets": {"encryption_keys": [ENCRYPTION_KEY]}},
        "auth": {"admin": {"bootstrap_token": TOKEN}},
        "providers": {"p": {"type": "openai", "base_url": "http://127.0.0.1:9/v1", "auth": {"api_key": "sk-up"}}},
        "deployments": {"p": {"provider": "p", "model": "up-model"}},
        "models": {"m": {"target": {"deployment": "p"}}}
    });
    let file = dir.path().join("node.json");
    std::fs::write(&file, doc.to_string()).unwrap();
    file
}

/// One HTTP/1.1 GET on a fresh connection: (status, body).
async fn get(addr: SocketAddr, path: &str, token: Option<&str>) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let auth = token.map_or(String::new(), |t| format!("authorization: Bearer {t}\r\n"));
    let request = format!("GET {path} HTTP/1.1\r\nhost: test\r\nconnection: close\r\n{auth}\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text.split(' ').nth(1).unwrap().parse().unwrap();
    let body = text.split_once("\r\n\r\n").map_or("", |(_, b)| b).to_owned();
    (status, body)
}

/// While the store does not answer: alive, not ready because of the store, and still starting.
async fn assert_waiting(data: SocketAddr, starting: &mut JoinSet<anyhow::Result<Node>>) {
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            starting.is_empty() || starting.try_join_next().is_none(),
            "the node stopped starting"
        );
    }
    assert_eq!(get(data, "/-/livez", None).await.0, 200);
    let (status, body) = get(data, "/-/readyz", None).await;
    assert_eq!(status, 503, "{body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["status"], "not_ready");
    assert_eq!(body["degraded"], json!(["store"]));
}

/// Once the store answers: the node finishes starting, is ready, and serves its startup document.
async fn assert_started(starting: &mut JoinSet<anyhow::Result<Node>>, data: SocketAddr, admin: SocketAddr) {
    let node = tokio::time::timeout(Duration::from_secs(60), starting.join_next())
        .await
        .expect("the node became ready within 60 s")
        .unwrap()
        .unwrap()
        .unwrap();
    let shutdown = node.rt.shutdown.clone();
    let mut serving = JoinSet::new();
    serving.spawn(node.run());
    let (status, body) = get(data, "/-/readyz", None).await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = get(admin, "/admin/v1/models/m", Some(TOKEN)).await;
    assert_eq!(status, 200, "the startup document is applied: {body}");
    shutdown.cancel();
    while serving.join_next().await.is_some() {}
}

/// Copies one direction of a connection, holding bytes while `frozen` is set.
async fn pump(
    mut from: impl AsyncRead + Unpin,
    mut to: impl AsyncWrite + Unpin,
    mut frozen: watch::Receiver<bool>,
) {
    let mut buf = vec![0u8; 16 << 10];
    loop {
        let Ok(n) = from.read(&mut buf).await else { return };
        if n == 0 || frozen.wait_for(|f| !*f).await.is_err() {
            return;
        }
        if to.write_all(&buf[..n]).await.is_err() {
            return;
        }
    }
}

/// A proxy to PostgreSQL that accepts connections but, while frozen, answers nothing.
async fn proxy(listener: TcpListener, upstream: SocketAddr, frozen: watch::Receiver<bool>) {
    let mut conns = JoinSet::new();
    while let Ok((client, _)) = listener.accept().await {
        let mut frozen = frozen.clone();
        conns.spawn(async move {
            if frozen.wait_for(|f| !*f).await.is_err() {
                return;
            }
            let Ok(server) = TcpStream::connect(upstream).await else {
                return;
            };
            let (cr, cw) = client.into_split();
            let (sr, sw) = server.into_split();
            tokio::join!(pump(cr, sw, frozen.clone()), pump(sr, cw, frozen));
        });
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_whose_store_accepts_but_does_not_answer_waits_not_ready_then_applies_its_startup_file() {
    let mut drivers = JoinSet::new();
    let Some(url) = fresh_database(&mut drivers).await else {
        return;
    };
    let dir = ScratchDir::new("cluster-start-silent");
    let pg: url_parts::Target = url_parts::Target::parse(&url);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxied = url.replacen(&pg.authority, &listener.local_addr().unwrap().to_string(), 1);
    let (freeze, frozen) = watch::channel(true);
    drivers.spawn(proxy(listener, pg.addr, frozen));
    let (data, admin) = (free_port(), free_port());
    let file = config(&dir, &proxied, data, admin);
    let mut starting = JoinSet::new();
    starting.spawn(start(ServeOptions {
        config: Some(file),
        test_mode: false,
    }));
    let (data, admin): (SocketAddr, SocketAddr) = (
        format!("127.0.0.1:{data}").parse().unwrap(),
        format!("127.0.0.1:{admin}").parse().unwrap(),
    );
    wait_bound(data).await;
    assert_waiting(data, &mut starting).await;
    freeze.send(false).unwrap();
    assert_started(&mut starting, data, admin).await;
    drivers.abort_all();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_store_that_stops_answering_after_the_node_opened_it_delays_readiness_instead_of_ending_the_node() {
    let mut drivers = JoinSet::new();
    let Some(url) = fresh_database(&mut drivers).await else {
        return;
    };
    migrate(&url).await;
    let (holder, conn) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    drivers.spawn(async move {
        let _ = conn.await;
    });
    // The first generation's write waits on this lock until the statement timeout, as on a store
    // that has just started and is too slow to answer.
    holder
        .batch_execute("BEGIN; LOCK TABLE generations IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let dir = ScratchDir::new("cluster-start-slow");
    let (data, admin) = (free_port(), free_port());
    let file = config(&dir, &url, data, admin);
    let mut starting = JoinSet::new();
    starting.spawn(start(ServeOptions {
        config: Some(file),
        test_mode: false,
    }));
    let (data, admin): (SocketAddr, SocketAddr) = (
        format!("127.0.0.1:{data}").parse().unwrap(),
        format!("127.0.0.1:{admin}").parse().unwrap(),
    );
    wait_bound(data).await;
    assert_waiting(data, &mut starting).await;
    holder.batch_execute("ROLLBACK").await.unwrap();
    assert_started(&mut starting, data, admin).await;
    drivers.abort_all();
}

async fn wait_bound(addr: SocketAddr) {
    for _ in 0..100 {
        if TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the node never bound {addr}");
}

mod url_parts {
    use std::net::{SocketAddr, ToSocketAddrs};

    /// The `host:port` of a PostgreSQL URL, as written and resolved.
    pub(crate) struct Target {
        pub(crate) authority: String,
        pub(crate) addr: SocketAddr,
    }

    impl Target {
        pub(crate) fn parse(url: &str) -> Target {
            let rest = url.split_once("://").unwrap().1;
            let host_part = rest.split('/').next().unwrap();
            let authority = host_part
                .rsplit_once('@')
                .map_or(host_part, |(_, h)| h)
                .to_owned();
            let addr = authority.to_socket_addrs().unwrap().next().unwrap();
            Target { authority, addr }
        }
    }
}

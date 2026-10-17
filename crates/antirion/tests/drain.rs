//! The drain as seen from a keep-alive HTTP/1.1 connection opened before it began: the drain is
//! driven through the node's drain state, the way the stop sequence drives it.

use std::net::SocketAddr;
use std::time::Duration;

use antirion::startup::{ServeOptions, start};
use antirion_runtime::Runtime;
use antirion_runtime::drain::Origin;
use antirion_testkit::ScratchDir;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::task::JoinSet;

const TOKEN: &str = "drain-test-bootstrap-token-0123456789abcdef";
const DRAIN_MIN: Duration = Duration::from_millis(300);

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// One keep-alive HTTP/1.1 connection, read response by response.
struct Conn(BufReader<TcpStream>);

impl Conn {
    async fn open(addr: SocketAddr) -> Conn {
        Conn(BufReader::new(TcpStream::connect(addr).await.unwrap()))
    }

    async fn send(&mut self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Response {
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nhost: test\r\ncontent-length: {}\r\n",
            body.len()
        );
        for (k, v) in headers {
            request.push_str(&format!("{k}: {v}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(body);
        self.0.get_mut().write_all(request.as_bytes()).await.unwrap();
        self.read_response().await
    }

    async fn line(&mut self) -> String {
        let mut line = String::new();
        let n = self.0.read_line(&mut line).await.unwrap();
        assert!(n > 0, "the connection closed before a response");
        line.trim_end().to_owned()
    }

    async fn read_response(&mut self) -> Response {
        let status = self.line().await.split(' ').nth(1).unwrap().parse().unwrap();
        let mut headers = Vec::new();
        loop {
            let line = self.line().await;
            if line.is_empty() {
                break;
            }
            let (k, v) = line.split_once(':').unwrap();
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
        }
        let mut response = Response {
            status,
            headers,
            body: String::new(),
        };
        let mut body = Vec::new();
        if let Some(len) = response.header("content-length") {
            body.resize(len.parse().unwrap(), 0);
            self.0.read_exact(&mut body).await.unwrap();
        } else {
            loop {
                let size = usize::from_str_radix(&self.line().await, 16).unwrap();
                let mut chunk = vec![0; size + 2];
                self.0.read_exact(&mut chunk).await.unwrap();
                if size == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..size]);
            }
        }
        response.body = String::from_utf8(body).unwrap();
        response
    }

    async fn closed_by_peer(&mut self) -> bool {
        let mut rest = Vec::new();
        matches!(
            tokio::time::timeout(Duration::from_secs(2), self.0.read_to_end(&mut rest)).await,
            Ok(Ok(0))
        )
    }
}

struct Running {
    rt: std::sync::Arc<Runtime>,
    data: SocketAddr,
    key: String,
    tasks: JoinSet<()>,
    _dir: ScratchDir,
}

impl Running {
    async fn start() -> Running {
        let dir = ScratchDir::new("drain");
        let config = json!({
            "version": 1,
            "server": {"listen": "127.0.0.1:0", "shutdown": {"drain_min": "300ms", "drain_max": "30s"}},
            "admin": {"listen": "127.0.0.1:0"},
            "store": {"embedded": {"path": dir.path().join("data").to_string_lossy()}},
            "auth": {"admin": {"bootstrap_token": TOKEN}}
        });
        let file = dir.path().join("gw.json");
        std::fs::write(&file, config.to_string()).unwrap();
        let node = start(ServeOptions {
            config: Some(file),
            test_mode: true,
        })
        .await
        .unwrap();
        let (data, admin) = (node.data_addr().unwrap(), node.admin_addr().unwrap());
        assert_eq!(node.rt.drain.settings.drain_min, DRAIN_MIN);
        let rt = std::sync::Arc::clone(&node.rt);
        let mut tasks = JoinSet::new();
        tasks.spawn(node.run());
        let bearer = format!("Bearer {TOKEN}");
        let created = Conn::open(admin)
            .await
            .send(
                "POST",
                "/admin/v1/keys",
                &[("authorization", &bearer), ("content-type", "application/json")],
                r#"{"name":"k","spec":{}}"#,
            )
            .await;
        assert_eq!(created.status, 201, "{}", created.body);
        let secret = serde_json::from_str::<Value>(&created.body).unwrap()["secret"]
            .as_str()
            .unwrap()
            .to_owned();
        Running {
            rt,
            data,
            key: format!("Bearer {secret}"),
            tasks,
            _dir: dir,
        }
    }

    /// Opens a connection and leaves it idle in keep-alive after one served request.
    async fn idle_connection(&self) -> Conn {
        let mut conn = Conn::open(self.data).await;
        let warm = self.models(&mut conn).await;
        assert_eq!(warm.status, 200, "{}", warm.body);
        assert_eq!(warm.header("connection"), None);
        conn
    }

    async fn models(&self, conn: &mut Conn) -> Response {
        conn.send("GET", "/v1/models", &[("authorization", &self.key)], "")
            .await
    }

    async fn stop(mut self) {
        self.rt.shutdown.cancel();
        while self.tasks.join_next().await.is_some() {}
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_connection_at_the_drain_start_serves_its_next_request_then_closes() {
    let node = Running::start().await;
    let mut conn = node.idle_connection().await;

    node.rt.drain.start(Origin::Signal);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let next = node.models(&mut conn).await;
    assert_eq!(next.status, 200, "{}", next.body);
    assert_eq!(next.header("connection"), Some("close"));
    assert!(
        conn.closed_by_peer().await,
        "the connection closes after that response"
    );

    let mut fresh = Conn::open(node.data).await;
    let during = node.models(&mut fresh).await;
    assert_eq!(during.status, 200, "new connections are served until drain_min");
    assert_eq!(during.header("connection"), Some("close"));
    node.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_on_an_open_connection_after_drain_min_is_refused_as_draining() {
    let node = Running::start().await;
    let mut conn = node.idle_connection().await;

    node.rt.drain.start(Origin::Signal);
    tokio::time::sleep(DRAIN_MIN).await;
    let late = node.models(&mut conn).await;
    assert_eq!(late.status, 503, "{}", late.body);
    let body: Value = serde_json::from_str(&late.body).unwrap();
    assert_eq!(body["antirion"]["code"], "draining", "{body}");
    assert_eq!(body["antirion"]["reason"], "node_draining", "{body}");
    assert_eq!(late.header("retry-after"), Some("1"));
    assert_eq!(late.header("connection"), Some("close"));
    assert!(conn.closed_by_peer().await);

    node.rt.drain.closed.cancel();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        TcpStream::connect(node.data).await.is_err(),
        "the listener is closed after drain_min"
    );
    node.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_connection_is_closed_with_a_fin_when_the_listeners_close() {
    let node = Running::start().await;
    let mut conn = node.idle_connection().await;

    node.rt.drain.start(Origin::Signal);
    tokio::time::sleep(DRAIN_MIN).await;
    node.rt.drain.closed.cancel();
    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_millis(500), conn.0.read_to_end(&mut rest)).await;
    assert!(
        matches!(read, Ok(Ok(0))),
        "the client sees an end of stream, not a reset or nothing: {read:?}"
    );
    assert!(!node.rt.shutdown.is_cancelled());
    node.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_racing_the_close_of_an_idle_connection_gets_a_503_or_a_clean_end() {
    const CONNECTIONS: u64 = 32;
    let node = Running::start().await;
    let mut conns = Vec::new();
    for _ in 0..CONNECTIONS {
        conns.push(node.idle_connection().await);
    }

    node.rt.drain.start(Origin::Signal);
    tokio::time::sleep(DRAIN_MIN).await;
    let mut senders = JoinSet::new();
    for (i, conn) in (0..CONNECTIONS).zip(conns) {
        let request = format!(
            "GET /v1/models HTTP/1.1\r\nhost: test\r\nauthorization: {}\r\n\r\n",
            node.key
        );
        senders.spawn(async move {
            let mut stream = conn.0.into_inner();
            tokio::time::sleep(Duration::from_millis(i)).await;
            let written = stream.write_all(request.as_bytes()).await;
            let mut answer = Vec::new();
            let read = tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut answer)).await;
            (written, read, answer)
        });
    }
    tokio::time::sleep(Duration::from_millis(CONNECTIONS / 2)).await;
    node.rt.drain.closed.cancel();

    let (mut refused, mut ended) = (0, 0);
    while let Some(outcome) = senders.join_next().await {
        let (written, read, answer) = outcome.unwrap();
        assert!(written.is_ok(), "the request was written: {written:?}");
        match read {
            Ok(Ok(0)) => ended += 1,
            Ok(Ok(_)) => {
                let text = String::from_utf8_lossy(&answer);
                assert!(text.starts_with("HTTP/1.1 503 "), "{text}");
                assert!(text.contains("node_draining"), "{text}");
                assert!(text.to_ascii_lowercase().contains("connection: close"), "{text}");
                refused += 1;
            }
            other => panic!("the connection ended without a clean close: {other:?}"),
        }
    }
    assert_eq!(refused + ended, CONNECTIONS);
    node.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_head_arriving_while_the_node_stops_is_answered() {
    let node = Running::start().await;
    let mut conn = node.idle_connection().await;
    let key = node.key.clone();
    let rt = std::sync::Arc::clone(&node.rt);

    conn.0
        .get_mut()
        .write_all(b"GET /v1/models HTTP/1.1\r\nhost: test\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    rt.drain.start(Origin::Signal);
    rt.drain.closed.cancel();
    let mut stopping = JoinSet::new();
    stopping.spawn(node.stop());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(rt.shutdown.is_cancelled());

    conn.0
        .get_mut()
        .write_all(format!("authorization: {key}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let late = conn.read_response().await;
    assert_eq!(late.status, 503, "{}", late.body);
    assert!(late.body.contains("node_draining"), "{}", late.body);
    assert_eq!(late.header("connection"), Some("close"));
    assert!(conn.closed_by_peer().await);
    stopping.join_next().await.unwrap().unwrap();
}

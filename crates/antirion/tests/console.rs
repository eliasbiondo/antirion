//! The console's static assets on the admin listener: where they are mounted, the headers they carry,
//! and their absence on the data listener and when the console is disabled.

use std::net::SocketAddr;

use antirion::startup::{ServeOptions, start};
use antirion_testkit::ScratchDir;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinSet;

const TOKEN: &str = "console-test-bootstrap-token-0123456789abcdef";

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

async fn exchange(addr: SocketAddr, method: &str, path: &str, auth: bool) -> Reply {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nhost: test\r\nconnection: close\r\ncontent-length: 0\r\n");
    if auth {
        request.push_str(&format!("authorization: Bearer {TOKEN}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Reply {
        status,
        headers,
        body: body.to_owned(),
    }
}

/// Runs `check` against a node whose `admin.ui` block is `ui`, then stops it.
async fn with_node<F, Fut>(ui: Value, check: F)
where
    F: FnOnce(SocketAddr, SocketAddr) -> Fut,
    Fut: Future<Output = ()>,
{
    let dir = ScratchDir::new("console");
    let config = json!({
        "version": 1,
        "server": {"listen": "127.0.0.1:0"},
        "admin": {"listen": "127.0.0.1:0", "ui": ui},
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
    let shutdown = node.rt.shutdown.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(node.run());
    check(data, admin).await;
    shutdown.cancel();
    while tasks.join_next().await.is_some() {}
}

fn assert_console_headers(r: &Reply) {
    let csp = r.header("content-security-policy").unwrap();
    assert!(csp.starts_with("default-src 'self'"), "{csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    assert_eq!(r.header("x-frame-options"), Some("DENY"));
    assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(r.header("referrer-policy"), Some("no-referrer"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_console_is_served_under_its_path_on_the_admin_listener_only() {
    with_node(json!({}), |data, admin| async move {
        for path in ["/ui/", "/ui"] {
            let r = exchange(admin, "GET", path, false).await;
            assert_eq!(r.status, 200, "{path}: {}", r.body);
            assert_eq!(r.header("content-type"), Some("text/html; charset=utf-8"));
            assert_eq!(r.header("cache-control"), Some("no-store"));
            assert!(r.body.contains("<html"), "{}", r.body);
            assert_console_headers(&r);
        }
        let head = exchange(admin, "HEAD", "/ui/", false).await;
        assert_eq!(head.status, 200);
        assert!(head.body.is_empty());

        let missing = exchange(admin, "GET", "/ui/no/such/page", false).await;
        assert_eq!(missing.status, 404);
        let post = exchange(admin, "POST", "/ui/", false).await;
        assert_eq!(post.status, 405);
        assert_eq!(post.header("allow"), Some("GET, HEAD"));
        assert_console_headers(&post);

        assert_eq!(exchange(data, "GET", "/ui/", false).await.status, 404);
        assert_eq!(exchange(data, "GET", "/ui", false).await.status, 404);
        assert_eq!(exchange(admin, "GET", "/uix", false).await.status, 404);

        let api = exchange(admin, "GET", "/admin/v1/status", true).await;
        assert_eq!(api.status, 200);
        assert_eq!(
            api.header("content-security-policy"),
            Some("frame-ancestors 'none'")
        );
        let status: Value = serde_json::from_str(&api.body).unwrap();
        assert_eq!(status["console"], json!({"enabled": true, "path": "/ui"}));
        assert_eq!(status["data_plane_url"], Value::Null);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_console_is_not_found() {
    with_node(json!({"enabled": false}), |_, admin| async move {
        for path in ["/ui", "/ui/", "/ui/index.html"] {
            assert_eq!(exchange(admin, "GET", path, false).await.status, 404, "{path}");
        }
        let status: Value =
            serde_json::from_str(&exchange(admin, "GET", "/admin/v1/status", true).await.body).unwrap();
        assert_eq!(status["console"]["enabled"], false);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_mount_follows_the_configured_path() {
    with_node(
        json!({"path": "/ops/console/", "data_plane_url": "https://gw.acme.example"}),
        |_, admin| async move {
            assert_eq!(exchange(admin, "GET", "/ops/console/", false).await.status, 200);
            assert_eq!(exchange(admin, "GET", "/ui/", false).await.status, 404);
            let status: Value =
                serde_json::from_str(&exchange(admin, "GET", "/admin/v1/status", true).await.body).unwrap();
            assert_eq!(status["console"]["path"], "/ops/console");
            assert_eq!(status["data_plane_url"], "https://gw.acme.example");
        },
    )
    .await;
}

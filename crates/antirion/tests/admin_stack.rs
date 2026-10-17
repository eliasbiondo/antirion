//! Stack headroom of the admin plane: its deepest operations run on worker threads with a stack
//! smaller than the gateway's own, so a handler that deepens the admin plane's frames fails here
//! before it can overflow a worker of the running gateway.

use std::net::SocketAddr;

use antirion::startup::{ServeOptions, start};
use antirion_testkit::ScratchDir;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinSet;

const TOKEN: &str = "stack-test-bootstrap-token-0123456789abcdef";

/// The worker stack the operations must fit in: a quarter of what the gateway gives its workers.
const SMALL_STACK: usize = antirion::WORKER_STACK / 4;

async fn call(
    admin: SocketAddr,
    method: &str,
    path: &str,
    extra: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut stream = TcpStream::connect(admin).await.unwrap();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nhost: test\r\nconnection: close\r\nauthorization: Bearer {TOKEN}\r\n\
         content-length: {}\r\n",
        body.len()
    );
    for (k, v) in extra {
        request.push_str(&format!("{k}: {v}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

/// `(method, path, headers, body, expected status)`.
type Operation<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)], &'a str, u16);

const JSON: (&str, &str) = ("content-type", "application/json");
const MERGE: (&str, &str) = ("content-type", "application/merge-patch+json");

async fn deepest_operations(dir: &ScratchDir) {
    let config = json!({
        "version": 1,
        "server": {"listen": "127.0.0.1:0"},
        "admin": {"listen": "127.0.0.1:0"},
        "store": {"embedded": {"path": dir.path().join("data").to_string_lossy()}},
        "auth": {"admin": {"bootstrap_token": TOKEN}},
        "providers": {"p": {"type": "openai", "base_url": "http://127.0.0.1:9/v1", "auth": {"api_key": "sk-up"}}},
        "deployments": {"d": {"provider": "p", "model": "up-model"}},
        "models": {"m": {"target": {"deployment": "d"}}},
        "observability": {"alerts": {"webhooks": {"hook": {"url": "http://127.0.0.1:9/hook", "events": ["*"]}}}}
    });
    let file = dir.path().join("gw.json");
    std::fs::write(&file, config.to_string()).unwrap();
    let node = start(ServeOptions {
        config: Some(file),
        test_mode: true,
    })
    .await
    .unwrap();
    let admin = node.admin_addr().unwrap();
    let shutdown = node.rt.shutdown.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(node.run());

    let (status, body) = call(admin, "GET", "/admin/v1/config", &[], "").await;
    assert_eq!(status, 200, "{body}");
    let mut doc: Value = serde_json::from_str(&body).unwrap();
    doc["models"]["m2"] = json!({"target": {"deployment": "d"}});
    let doc = doc.to_string();
    let operations: [Operation; 9] = [
        (
            "POST",
            "/admin/v1/alerts/webhooks/hook:rotate_secret",
            &[JSON],
            r#"{"overlap":"10m"}"#,
            200,
        ),
        ("POST", "/admin/v1/config:validate", &[JSON], &doc, 200),
        ("POST", "/admin/v1/config:apply", &[JSON], &doc, 200),
        (
            "POST",
            "/admin/v1/keys",
            &[JSON, ("idempotency-key", "stack-1")],
            r#"{"name":"k","spec":{}}"#,
            201,
        ),
        ("POST", "/admin/v1/keys/k:rotate", &[JSON], "{}", 200),
        (
            "PATCH",
            "/admin/v1/settings/pricing",
            &[MERGE],
            r#"{"spec":{"on_unpriced":"zero"}}"#,
            200,
        ),
        (
            "PUT",
            "/admin/v1/routes/r",
            &[JSON],
            r#"{"spec":{"tree":"d"}}"#,
            201,
        ),
        (
            "POST",
            "/admin/v1/settings/security:rotate_secrets",
            &[JSON],
            r#"{"scope":["scim_token"]}"#,
            200,
        ),
        ("DELETE", "/admin/v1/models/m2", &[], "", 204),
    ];
    for (method, path, headers, body, expected) in operations {
        let (status, answer) = call(admin, method, path, headers, body).await;
        assert_eq!(status, expected, "{method} {path}: {answer}");
    }

    shutdown.cancel();
    while tasks.join_next().await.is_some() {}
}

#[test]
fn the_deepest_admin_operations_fit_in_a_quarter_of_a_worker_stack() {
    let dir = ScratchDir::new("admin-stack");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(SMALL_STACK)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(deepest_operations(&dir));
}

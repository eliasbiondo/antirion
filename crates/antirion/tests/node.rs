//! A whole node in process: configured from a file, driven over its two listeners, forwarding to a
//! scripted upstream.

use std::net::SocketAddr;
use std::time::Duration;

use antirion::startup::{ServeOptions, start};
use antirion_testkit::ScratchDir;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

const TOKEN: &str = "node-test-bootstrap-token-0123456789abcdef";

/// One HTTP/1.1 exchange on a fresh connection: (status, lowercase headers, body).
async fn exchange(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, Vec<(String, String)>, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nhost: test\r\nconnection: close\r\ncontent-length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        request.push_str(&format!("{k}: {v}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, rest) = text.split_once("\r\n\r\n").unwrap();
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let chunked = headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.contains("chunked"));
    let body = if chunked { dechunk(rest) } else { rest.to_owned() };
    (status, headers, body)
}

fn dechunk(mut s: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = s.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.push_str(&rest[..n]);
        s = &rest[n + 2..];
    }
    out
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// An upstream that answers one chat completion and reports the request it received.
async fn upstream(listener: TcpListener) -> String {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).await.unwrap();
        seen.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&seen);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let len: usize = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap_or(0);
            if body.len() >= len {
                break;
            }
        }
    }
    let reply = r#"{"id":"c1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
        reply.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
    String::from_utf8_lossy(&seen).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_serves_keys_models_and_a_passthrough() {
    let dir = ScratchDir::new("node");
    let mock = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock.local_addr().unwrap();
    let config = json!({
        "version": 1,
        "server": {"listen": "127.0.0.1:0"},
        "admin": {"listen": "127.0.0.1:0"},
        "store": {"embedded": {"path": dir.path().join("data").to_string_lossy()}},
        "auth": {"admin": {"bootstrap_token": TOKEN}},
        "providers": {"p": {"type": "openai", "base_url": format!("http://{mock_addr}/v1"), "auth": {"api_key": "sk-up"}}},
        "deployments": {"p": {"provider": "p", "model": "up-model"}},
        "models": {"m": {"target": {"deployment": "p"}}}
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
    let mut upstream_task = JoinSet::new();
    upstream_task.spawn(upstream(mock));

    let (status, headers, body) = exchange(data, "GET", "/-/readyz", &[], "").await;
    assert_eq!(status, 200);
    assert_eq!(header(&headers, "x-antirion-test-mode"), Some("true"));
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["test_mode"], true);

    let bearer = format!("Bearer {TOKEN}");
    let auth = [
        ("authorization", bearer.as_str()),
        ("content-type", "application/json"),
    ];
    let (status, _, body) = exchange(
        admin,
        "POST",
        "/admin/v1/keys",
        &auth,
        r#"{"name":"k","spec":{}}"#,
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let secret = serde_json::from_str::<Value>(&body).unwrap()["secret"]
        .as_str()
        .unwrap()
        .to_owned();

    let (status, headers, body) = exchange(
        data,
        "POST",
        "/v1/chat/completions",
        &[("content-type", "application/json")],
        "{}",
    )
    .await;
    assert_eq!(status, 401);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["antirion"]["reason"],
        "missing_api_key"
    );
    assert!(header(&headers, "www-authenticate").is_some_and(|v| v.starts_with("Bearer")));

    let key = format!("Bearer {secret}");
    let chat = r#"{"model":"m","messages":[{"role":"user","content":"hello"}]}"#;
    let (status, headers, body) = exchange(
        data,
        "POST",
        "/v1/chat/completions",
        &[("authorization", key.as_str())],
        chat,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(header(&headers, "x-antirion-deployment"), Some("p"));
    assert!(body.contains("\"hi\""));
    let seen = tokio::time::timeout(Duration::from_secs(5), upstream_task.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .to_ascii_lowercase();
    assert!(seen.contains("authorization: bearer sk-up"), "{seen}");
    assert!(seen.contains("\"model\":\"up-model\""), "{seen}");
    assert!(
        !seen.contains(&secret.to_ascii_lowercase()),
        "the client's key never reaches the upstream"
    );

    shutdown.cancel();
    while tasks.join_next().await.is_some() {}
}

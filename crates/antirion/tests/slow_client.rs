//! A client that stops reading a stream for a while: the gateway waits for it (up to
//! `streaming.client_write_timeout`) instead of reading on, and that wait is not upstream silence, so
//! `stream_idle` must not end the stream or count against the provider.

use std::net::SocketAddr;
use std::time::Duration;

use antirion::startup::{ServeOptions, start};
use antirion_testkit::ScratchDir;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

const TOKEN: &str = "slow-client-bootstrap-token-0123456789abcdef01234";
/// Events the upstream sends at once: far more than the send buffer and the sockets hold.
const EVENTS: usize = 2_000;
const TEXT: usize = 4_000;

async fn request(addr: SocketAddr, method: &str, path: &str, auth: &str, body: &str) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nhost: test\r\nconnection: close\r\ncontent-type: application/json\r\n\
         authorization: {auth}\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream
}

async fn read_all(mut stream: TcpStream) -> String {
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    String::from_utf8_lossy(&raw).into_owned()
}

/// An upstream that answers every chat request with a long stream, written as fast as it is read.
async fn upstream(listener: TcpListener) {
    let mut connections = JoinSet::new();
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        connections.spawn(async move {
            let mut buf = vec![0u8; 16_384];
            let mut seen = Vec::new();
            loop {
                let Ok(n) = stream.read(&mut buf).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                seen.extend_from_slice(&buf[..n]);
                let Some(end) = seen.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head = String::from_utf8_lossy(&seen[..end]).to_ascii_lowercase();
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .map_or(0, |v| v.trim().parse().unwrap_or(0));
                if seen.len() >= end + 4 + len {
                    break;
                }
            }
            let reply = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
            if stream.write_all(reply.as_bytes()).await.is_err() {
                return;
            }
            let piece = "x".repeat(TEXT);
            for i in 0..EVENTS {
                let delta = if i == 0 { json!({"role": "assistant", "content": ""}) } else { json!({"content": piece}) };
                let event = json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "up-gpt",
                    "choices": [{"index": 0, "delta": delta, "finish_reason": null}]});
                if stream.write_all(format!("data: {event}\n\n").as_bytes()).await.is_err() {
                    return;
                }
            }
            let last = json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "up-gpt",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 5, "completion_tokens": 9, "total_tokens": 14}});
            let _ = stream.write_all(format!("data: {last}\n\ndata: [DONE]\n\n").as_bytes()).await;
        });
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_pauses_reading_is_not_an_idle_upstream() {
    let dir = ScratchDir::new("slow-client");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
    let mut mock_task = JoinSet::new();
    mock_task.spawn(upstream(listener));

    let config = json!({
        "version": 1,
        "server": {"listen": "127.0.0.1:0"},
        "admin": {"listen": "127.0.0.1:0"},
        "store": {"embedded": {"path": dir.path().join("data").to_string_lossy()}},
        "auth": {"admin": {"bootstrap_token": TOKEN}},
        "streaming": {"send_buffer": "64KiB", "client_write_timeout": "20s"},
        "providers": {"oa": {"type": "openai", "base_url": format!("http://{mock}/v1"), "auth": {"api_key": "sk-up"}}},
        "deployments": {"oa": {"provider": "oa", "model": "up-gpt"}},
        "models": {"gpt": {"target": {"deployment": "oa"}}},
        "policies": {"resilience": {"timeouts": {"stream_idle": "1s"}}}
    });
    let file = dir.path().join("gw.json");
    std::fs::write(&file, config.to_string()).unwrap();
    let node = start(ServeOptions {
        config: Some(file),
        test_mode: true,
        no_config: None,
    })
    .await
    .unwrap();
    let (data, admin) = (node.data_addr().unwrap(), node.admin_addr().unwrap());
    let shutdown = node.rt.shutdown.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let _ = node.run().await;
    });

    let bearer = format!("Bearer {TOKEN}");
    let created = read_all(
        request(
            admin,
            "POST",
            "/admin/v1/keys",
            &bearer,
            r#"{"name":"k","spec":{}}"#,
        )
        .await,
    )
    .await;
    let key = created
        .split_once("\r\n\r\n")
        .and_then(|(_, b)| serde_json::from_str::<Value>(b).ok())
        .and_then(|v| v["secret"].as_str().map(str::to_owned))
        .expect("a key");

    let chat = json!({"model": "gpt", "stream": true, "messages": [{"role": "user", "content": "go"}]});
    let stream = request(
        data,
        "POST",
        "/v1/chat/completions",
        &format!("Bearer {key}"),
        &chat.to_string(),
    )
    .await;
    // The client stops reading for three idle timeouts while the gateway holds a full buffer.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let answer = read_all(stream).await;
    assert!(
        answer.starts_with("HTTP/1.1 200"),
        "{}",
        &answer[..answer.len().min(300)]
    );
    assert!(
        !answer.contains("upstream_idle_timeout"),
        "the stream was cut as idle"
    );
    assert!(answer.contains("data: [DONE]"), "the stream did not complete");

    let health = read_all(request(admin, "GET", "/admin/v1/runtime/deployments/oa", &bearer, "").await).await;
    assert!(
        health.starts_with("HTTP/1.1 200 "),
        "deployment health request failed: {health}"
    );
    let (_, body) = health
        .split_once("\r\n\r\n")
        .expect("a deployment health response body");
    let health: Value = serde_json::from_str(body).expect("a JSON deployment health response");
    assert_eq!(health["name"], "oa");
    assert_eq!(
        health["window"]["failures"], 0,
        "the provider was blamed: {health}"
    );

    shutdown.cancel();
    while tasks.join_next().await.is_some() {}
    mock_task.abort_all();
}

/// An upstream that sends one event and then goes silent, its connection held open.
async fn stalling(listener: TcpListener) {
    let mut connections = JoinSet::new();
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        connections.spawn(async move {
            let mut buf = vec![0u8; 16_384];
            let _ = stream.read(&mut buf).await;
            let first = json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "up-gpt",
                "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Hel"}, "finish_reason": null}]});
            let reply = format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {first}\n\n");
            let _ = stream.write_all(reply.as_bytes()).await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_silent_upstream_still_ends_the_stream_as_idle() {
    let dir = ScratchDir::new("silent-upstream");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
    let mut mock_task = JoinSet::new();
    mock_task.spawn(stalling(listener));

    let config = json!({
        "version": 1,
        "server": {"listen": "127.0.0.1:0"},
        "admin": {"listen": "127.0.0.1:0"},
        "store": {"embedded": {"path": dir.path().join("data").to_string_lossy()}},
        "auth": {"admin": {"bootstrap_token": TOKEN}},
        "providers": {"oa": {"type": "openai", "base_url": format!("http://{mock}/v1"), "auth": {"api_key": "sk-up"}}},
        "deployments": {"oa": {"provider": "oa", "model": "up-gpt"}},
        "models": {"gpt": {"target": {"deployment": "oa"}}},
        "policies": {"resilience": {"timeouts": {"stream_idle": "1s"}}}
    });
    let file = dir.path().join("gw.json");
    std::fs::write(&file, config.to_string()).unwrap();
    let node = start(ServeOptions {
        config: Some(file),
        test_mode: true,
        no_config: None,
    })
    .await
    .unwrap();
    let (data, admin) = (node.data_addr().unwrap(), node.admin_addr().unwrap());
    let shutdown = node.rt.shutdown.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let _ = node.run().await;
    });

    let bearer = format!("Bearer {TOKEN}");
    let created = read_all(
        request(
            admin,
            "POST",
            "/admin/v1/keys",
            &bearer,
            r#"{"name":"k","spec":{}}"#,
        )
        .await,
    )
    .await;
    let key = created
        .split_once("\r\n\r\n")
        .and_then(|(_, b)| serde_json::from_str::<Value>(b).ok())
        .and_then(|v| v["secret"].as_str().map(str::to_owned))
        .expect("a key");

    let chat = json!({"model": "gpt", "stream": true, "messages": [{"role": "user", "content": "go"}]});
    let started = std::time::Instant::now();
    let stream = request(
        data,
        "POST",
        "/v1/chat/completions",
        &format!("Bearer {key}"),
        &chat.to_string(),
    )
    .await;
    let answer = tokio::time::timeout(Duration::from_secs(10), read_all(stream))
        .await
        .expect("the stream ended");
    assert!(answer.contains("upstream_idle_timeout"), "{answer}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "ended after {:?}",
        started.elapsed()
    );

    shutdown.cancel();
    while tasks.join_next().await.is_some() {}
    mock_task.abort_all();
}

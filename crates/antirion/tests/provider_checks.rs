//! Provider checks over a whole node: a credential test without a deployment lists the provider's
//! models, its outcome shows in the provider's runtime status, a provider is tested before it is
//! saved, and the model list is kept, filled in from the catalog, or replaced by the catalog's.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use antirion::startup::{ServeOptions, start};
use antirion_testkit::ScratchDir;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

const TOKEN: &str = "provider-checks-bootstrap-token-0123456789abcdef";
const GOOD: &str = "sk-good-key-0123456789";
const BAD: &str = "sk-bad-key-0123456789";

async fn admin(addr: SocketAddr, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
    let body = body.map(Value::to_string).unwrap_or_default();
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nhost: test\r\nconnection: close\r\nauthorization: Bearer {TOKEN}\r\n\
         content-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let rest = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        dechunk(rest)
    } else {
        rest.to_owned()
    };
    (status, serde_json::from_str(&rest).unwrap_or(Value::Null))
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

/// A model-list response for one of the paginated Gemini fixtures.
fn paginated_listing(path: &str) -> Option<Value> {
    let (route, query) = path.split_once('?')?;
    let cursor = query
        .split('&')
        .find_map(|param| param.strip_prefix("pageToken="));
    let page = match (route, cursor) {
        ("/page-cap/v1beta/models", cursor) => {
            let page = cursor.unwrap_or("0").parse::<usize>().ok()?;
            json!({"models": [{"name": format!("models/page-{page:02}")}], "nextPageToken": (page + 1).to_string()})
        }
        ("/repeated-cursor/v1beta/models", None) => {
            json!({"models": [{"name": "models/first"}], "nextPageToken": "repeat"})
        }
        ("/repeated-cursor/v1beta/models", Some("repeat")) => {
            json!({"models": [{"name": "models/second"}], "nextPageToken": "repeat"})
        }
        ("/empty-page/v1beta/models", None) => {
            json!({"models": [{"name": "models/first"}], "nextPageToken": "empty"})
        }
        ("/empty-page/v1beta/models", Some("empty")) => {
            json!({"models": [], "nextPageToken": "last"})
        }
        ("/empty-page/v1beta/models", Some("last")) => {
            json!({"models": [{"name": "models/last"}]})
        }
        _ => return None,
    };
    Some(page)
}

/// An upstream that serves the model-list fixtures and counts successful listing requests.
async fn upstream(listener: TcpListener, listings: Arc<AtomicUsize>) {
    let mut connections = JoinSet::new();
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let listings = Arc::clone(&listings);
        connections.spawn(async move {
            let mut seen: Vec<u8> = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let text = String::from_utf8_lossy(&seen).into_owned();
                let Some((head, body)) = text.split_once("\r\n\r\n") else {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => seen.extend_from_slice(&buf[..n]),
                    }
                    continue;
                };
                let lower = head.to_ascii_lowercase();
                let len: usize = lower
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if body.len() < len {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => seen.extend_from_slice(&buf[..n]),
                    }
                    continue;
                }
                let consumed = head.len() + 4 + len;
                seen.drain(..consumed);
                let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
                let (status, reply) = if path == "/models-dev/api.json" {
                    (
                        200,
                        json!({"anthropic": {"models": {"claude-remote-1": {"id": "claude-remote-1",
                            "name": "Claude Remote", "cost": {"input": 1.5, "output": 7.5}}}}}),
                    )
                } else if let Some(page) = paginated_listing(&path) {
                    listings.fetch_add(1, Ordering::SeqCst);
                    (200, page)
                } else if path.starts_with("/anthropic/") {
                    (
                        500,
                        json!({"error": {"type": "api_error", "message": "listing is down"}}),
                    )
                } else if path == "/v1/models" && lower.contains(&format!("authorization: bearer {GOOD}")) {
                    listings.fetch_add(1, Ordering::SeqCst);
                    (
                        200,
                        json!({"object": "list", "data": [
                            {"id": "own-model", "object": "model", "owned_by": "me"},
                            {"id": "gpt-4o-mini", "object": "model", "owned_by": "me"}
                        ]}),
                    )
                } else if path == "/v1/models" {
                    (
                        401,
                        json!({"error": {"message": format!("Incorrect API key provided: {BAD}")}}),
                    )
                } else {
                    (404, json!({"error": {"message": "no such path"}}))
                };
                let reply = reply.to_string();
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
                    reply.len()
                );
                if stream.write_all(response.as_bytes()).await.is_err() {
                    return;
                }
            }
        });
    }
}

fn ids(items: &Value) -> Vec<&str> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_model_listing_stops_at_the_cap_or_a_repeated_cursor_and_follows_empty_pages() {
    let dir = ScratchDir::new("provider-pagination");
    let mock = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock.local_addr().unwrap();
    let listings = Arc::new(AtomicUsize::new(0));
    let mut mock_task = JoinSet::new();
    mock_task.spawn(upstream(mock, Arc::clone(&listings)));

    let config = json!({
        "version": 1,
        "server": {"listen": "127.0.0.1:0"},
        "admin": {"listen": "127.0.0.1:0"},
        "store": {"embedded": {"path": dir.path().join("data").to_string_lossy()}},
        "auth": {"admin": {"bootstrap_token": TOKEN}},
        "providers": {
            "page-cap": {"type": "gemini", "base_url": format!("http://{mock_addr}/page-cap"), "auth": {"api_key": GOOD}},
            "repeated-cursor": {"type": "gemini", "base_url": format!("http://{mock_addr}/repeated-cursor"), "auth": {"api_key": GOOD}},
            "empty-page": {"type": "gemini", "base_url": format!("http://{mock_addr}/empty-page"), "auth": {"api_key": GOOD}}
        }
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
    let addr = node.admin_addr().unwrap();
    let shutdown = node.rt.shutdown.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(node.run());

    let capped: Vec<String> = (0..20).map(|page| format!("page-{page:02}")).collect();
    let cases = [
        ("page-cap", 20, capped.iter().map(String::as_str).collect()),
        ("repeated-cursor", 2, vec!["first", "second"]),
        ("empty-page", 3, vec!["first", "last"]),
    ];
    for (provider, requests, expected) in cases {
        let before = listings.load(Ordering::SeqCst);
        let path = format!("/admin/v1/providers/{provider}/models");
        let (status, models) = admin(addr, "GET", &path, None).await;
        assert_eq!(status, 200, "{provider}: {models}");
        assert_eq!(models["source"], "provider", "{provider}: {models}");
        assert_eq!(ids(&models["items"]), expected, "{provider}");
        assert_eq!(listings.load(Ordering::SeqCst) - before, requests, "{provider}");
    }

    shutdown.cancel();
    while tasks.join_next().await.is_some() {}
    mock_task.abort_all();
}

#[tokio::test(flavor = "multi_thread")]
async fn tests_lists_and_status_of_providers() {
    let dir = ScratchDir::new("provider-checks");
    let mock = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock.local_addr().unwrap();
    let listings = Arc::new(AtomicUsize::new(0));
    let mut mock_task = JoinSet::new();
    mock_task.spawn(upstream(mock, Arc::clone(&listings)));
    let base = format!("http://{mock_addr}/v1");
    let config = json!({
        "version": 1,
        "server": {"listen": "127.0.0.1:0"},
        "admin": {"listen": "127.0.0.1:0"},
        "store": {"embedded": {"path": dir.path().join("data").to_string_lossy()}},
        "auth": {"admin": {"bootstrap_token": TOKEN}},
        "providers": {
            "p": {"type": "openai", "base_url": base,
                  "auth": {"api_keys": [{"name": "good", "key": GOOD}, {"name": "bad", "key": BAD, "weight": 0}]}},
            "a": {"type": "anthropic", "base_url": format!("http://{mock_addr}/anthropic"), "auth": {"api_key": GOOD}}
        }
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
    let addr = node.admin_addr().unwrap();
    let shutdown = node.rt.shutdown.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(node.run());

    // Before any test, nothing is recorded.
    let (status, p) = admin(addr, "GET", "/admin/v1/runtime/providers/p", None).await;
    assert_eq!(status, 200, "{p}");
    assert_eq!(p["credential"]["state"], "valid");
    assert!(p["credential"].get("last_test").is_none());
    assert_eq!(p["tests"], json!([]));

    // A test without a body and without any deployment lists the models.
    let (status, t) = admin(addr, "POST", "/admin/v1/providers/p:test", None).await;
    assert_eq!(status, 200, "{t}");
    assert_eq!(t["ok"], true, "{t}");
    assert_eq!(t["method"], "list_models");
    assert_eq!(t["status_code"], 200);
    assert_eq!(t["error"], Value::Null);
    assert!(t["tested_at"].as_str().is_some() && t["latency_ms"].is_number());
    assert_eq!(ids(&t["models"]), ["gpt-4o-mini", "own-model"]);
    let mini = &t["models"][0];
    assert_eq!(mini["listed_by_provider"], true);
    assert_eq!(mini["price"]["input"], "0.15", "the bundled price");
    assert_eq!(mini["price_source"], "catalog");
    assert_eq!(mini["name"], "GPT-4o mini", "models.dev's name");
    assert_eq!(mini["context_window"], 128_000);
    assert_eq!(t["models"][1]["price"], Value::Null);

    // The status shows the outcome.
    let (_, p) = admin(addr, "GET", "/admin/v1/runtime/providers/p", None).await;
    assert_eq!(p["credential"]["state"], "valid");
    assert_eq!(p["credential"]["last_test"]["ok"], true, "{p}");
    assert_eq!(p["credential"]["last_test"]["key"], "good");
    assert_eq!(p["credential"]["last_test"]["method"], "list_models");
    assert_eq!(p["tests"].as_array().unwrap().len(), 1);

    // A failing key: the error is scrubbed of the provider's secrets and the status says failed.
    let (status, t) = admin(
        addr,
        "POST",
        "/admin/v1/providers/p:test",
        Some(&json!({"credential": "bad"})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        (t["ok"].clone(), t["status_code"].clone()),
        (json!(false), json!(401))
    );
    let error = t["error"].as_str().unwrap();
    assert!(error.contains("[REDACTED]") && !error.contains(BAD), "{error}");
    assert_eq!(t["models"], Value::Null);
    let (_, list) = admin(addr, "GET", "/admin/v1/runtime/providers", None).await;
    let p = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "p")
        .unwrap();
    assert_eq!(p["credential"]["state"], "failed");
    assert_eq!(p["credential"]["last_test"]["key"], "bad");
    assert!(
        p["credential"]["last_error"]
            .as_str()
            .unwrap()
            .contains("[REDACTED]")
    );
    assert_eq!(p["tests"].as_array().unwrap().len(), 2);
    assert!(!list.to_string().contains(BAD));
    let (status, e) = admin(
        addr,
        "POST",
        "/admin/v1/providers/p:test",
        Some(&json!({"credential": "nope"})),
    )
    .await;
    assert_eq!(status, 400, "{e}");
    let (status, _) = admin(addr, "POST", "/admin/v1/providers/none:test", None).await;
    assert_eq!(status, 404);

    // The model list is kept: the test's listing serves the next read.
    let before = listings.load(Ordering::SeqCst);
    let (status, m) = admin(addr, "GET", "/admin/v1/providers/p/models", None).await;
    assert_eq!(status, 200, "{m}");
    assert_eq!(m["source"], "provider");
    assert_eq!(m["error"], Value::Null);
    assert!(m["fetched_at"].as_str().is_some());
    assert_eq!(ids(&m["items"]), ["gpt-4o-mini", "own-model"]);
    assert_eq!(
        listings.load(Ordering::SeqCst),
        before,
        "served from the kept list"
    );
    let (_, m) = admin(addr, "GET", "/admin/v1/providers/p/models?refresh=true", None).await;
    assert_eq!(m["source"], "provider");
    assert_eq!(listings.load(Ordering::SeqCst), before + 1, "refresh lists again");
    let (_, m) = admin(addr, "GET", "/admin/v1/providers/p/models?q=MINI", None).await;
    assert_eq!(ids(&m["items"]), ["gpt-4o-mini"], "q matches the name too");
    let (_, m) = admin(addr, "GET", "/admin/v1/providers/p/models?id=gpt-4o", None).await;
    assert_eq!(
        ids(&m["items"]),
        ["gpt-4o"],
        "the catalog knows a model the provider did not list"
    );
    assert_eq!(m["items"][0]["listed_by_provider"], false);
    assert_eq!(m["items"][0]["price"]["output"], "10.00");
    let (_, m) = admin(addr, "GET", "/admin/v1/providers/p/models?id=no-such-model", None).await;
    assert_eq!(m["items"], json!([]));
    let (status, _) = admin(addr, "GET", "/admin/v1/providers/p/models?x=1", None).await;
    assert_eq!(status, 400);

    // A provider whose listing fails falls back to the catalog's models of its type.
    let (status, m) = admin(addr, "GET", "/admin/v1/providers/a/models", None).await;
    assert_eq!(status, 200, "{m}");
    assert_eq!(m["source"], "catalog");
    assert_eq!(m["fetched_at"], Value::Null);
    assert!(m["error"].as_str().unwrap().contains("listing is down"), "{m}");
    let items = m["items"].as_array().unwrap();
    assert!(items.len() > 5);
    assert!(items.iter().all(|i| i["listed_by_provider"] == false));
    assert!(
        items
            .iter()
            .any(|i| i["id"] == "claude-sonnet-4-5" && i["price"]["input"] == "3.00")
    );

    // A provider not saved yet: tested as a create would compile it, and never stored.
    let spec = json!({"type": "openai", "base_url": base, "auth": {"api_key": GOOD}});
    let (status, t) = admin(
        addr,
        "POST",
        "/admin/v1/providers:test",
        Some(&json!({"name": "draft", "spec": spec})),
    )
    .await;
    assert_eq!(status, 200, "{t}");
    assert_eq!(
        (t["ok"].clone(), t["method"].clone()),
        (json!(true), json!("list_models"))
    );
    assert_eq!(ids(&t["models"]), ["gpt-4o-mini", "own-model"]);
    let (status, _) = admin(addr, "GET", "/admin/v1/providers/draft", None).await;
    assert_eq!(status, 404, "an unsaved test stores nothing");
    let (_, rt) = admin(addr, "GET", "/admin/v1/runtime/providers", None).await;
    assert!(
        rt["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["name"] != "draft")
    );
    let (status, e) = admin(
        addr,
        "POST",
        "/admin/v1/providers:test",
        Some(&json!({"name": "draft", "spec": {"type": "no_such_type"}})),
    )
    .await;
    assert_eq!(status, 400, "{e}");
    assert_eq!(e["error"]["code"], "validation_failed");
    assert!(
        e["error"]["details"][0]["path"]
            .as_str()
            .unwrap()
            .starts_with("providers.draft")
    );
    let (status, e) = admin(
        addr,
        "POST",
        "/admin/v1/providers:test",
        Some(&json!({"spec": spec})),
    )
    .await;
    assert_eq!(status, 400, "{e}");
    let (status, e) = admin(
        addr,
        "POST",
        "/admin/v1/providers:test",
        Some(
            &json!({"name": "draft", "spec": {"type": "openai", "base_url": base,
                     "auth": {"api_key": "${env:ANTIRION_PROVIDER_CHECKS_UNSET}"}}}),
        ),
    )
    .await;
    assert_eq!(status, 400, "an unresolved reference fails as in a create: {e}");
    let elsewhere = json!({"name": "p", "spec": {"type": "openai", "base_url": "http://127.0.0.1:9/v1",
                           "auth": {"api_keys": [{"name": "good", "key": "[REDACTED]"}]}}});
    let (status, e) = admin(addr, "POST", "/admin/v1/providers:test", Some(&elsewhere)).await;
    assert_eq!(status, 400, "a saved secret goes only to the saved base URL: {e}");
    let same = json!({"name": "p", "spec": {"type": "openai", "base_url": base,
                      "auth": {"api_keys": [{"name": "good", "key": "[REDACTED]"}]}}});
    let (status, t) = admin(addr, "POST", "/admin/v1/providers:test", Some(&same)).await;
    assert_eq!(status, 200, "{t}");
    assert_eq!(t["ok"], true, "the saved secret, at the saved base URL: {t}");

    // The audit records the tests, never a secret.
    let (status, audit) = admin(addr, "GET", "/admin/v1/audit?limit=100", None).await;
    assert_eq!(status, 200, "{audit}");
    let text = audit.to_string();
    assert!(text.contains("POST providers:test"));
    assert!(text.contains("providers/draft"));
    assert!(!text.contains(GOOD) && !text.contains(BAD));

    // `catalog.remote` puts a fetched models.dev document in place of the bundled snapshot.
    let remote = json!({"spec": {"remote": {"enabled": true, "refresh": "1h",
                        "url": format!("http://{mock_addr}/models-dev/api.json")}}});
    let (status, e) = admin(addr, "PATCH", "/admin/v1/settings/catalog", Some(&remote)).await;
    assert_eq!(status, 200, "{e}");
    let mut seen = Value::Null;
    for _ in 0..100 {
        let (_, m) = admin(
            addr,
            "GET",
            "/admin/v1/providers/a/models?id=claude-remote-1",
            None,
        )
        .await;
        if m["items"].as_array().is_some_and(|i| !i.is_empty()) {
            seen = m;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert_eq!(seen["items"][0]["price"]["input"], "1.5", "{seen}");
    assert_eq!(seen["items"][0]["name"], "Claude Remote");
    let bad = json!({"spec": {"remote": {"enabled": true, "refresh": "1m"}}});
    let (status, _) = admin(addr, "PATCH", "/admin/v1/settings/catalog", Some(&bad)).await;
    assert_eq!(status, 400, "a refresh under an hour is refused");
    let off = json!({"spec": {"remote": {"enabled": false}}});
    let (status, _) = admin(addr, "PATCH", "/admin/v1/settings/catalog", Some(&off)).await;
    assert_eq!(status, 200);
    let mut back = false;
    for _ in 0..100 {
        let (_, m) = admin(
            addr,
            "GET",
            "/admin/v1/providers/a/models?id=claude-remote-1",
            None,
        )
        .await;
        if m["items"] == json!([]) {
            back = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert!(back, "turning it off goes back to the bundled snapshot");

    shutdown.cancel();
    while tasks.join_next().await.is_some() {}
    mock_task.abort_all();
}

//! Tool-definition approvals outlive a restart: an approved server's tools stay visible, a changed
//! definition is hidden until approved again, and the test reset forgets the stored approvals.

use std::net::SocketAddr;

use antirion::startup::{ServeOptions, start};
use antirion_testkit::ScratchDir;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinSet;

const TOKEN: &str = "mcp-approvals-bootstrap-token-0123456789abcdef";

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
        "{method} {path} HTTP/1.1\r\nhost: test\r\nconnection: close\r\ncontent-type: application/json\r\n\
         content-length: {}\r\n",
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
        let n = usize::from_str_radix(size.split(';').next().unwrap_or("0").trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.push_str(&rest[..n]);
        s = &rest[n + 2..];
    }
    out
}

/// The inline OpenAPI document of `audit_changes.rs`, with two operations and a replaceable summary.
fn spec(summary: &str) -> Value {
    json!({"openapi": "3.1.0", "info": {"title": "crm", "version": "1"},
    "paths": {
        "/customers": {"get": {"operationId": "search_customers", "summary": summary,
            "responses": {"200": {"description": "ok"}}}},
        "/orders": {"get": {"operationId": "list_orders", "summary": "List orders.",
            "responses": {"200": {"description": "ok"}}}}
    }})
}

fn config(dir: &ScratchDir, spec: &Value) -> Value {
    json!({
        "version": 1,
        "server": {"listen": "127.0.0.1:0"},
        "admin": {"listen": "127.0.0.1:0"},
        "store": {"embedded": {"path": dir.path().join("data").to_string_lossy()}},
        "auth": {"admin": {"bootstrap_token": TOKEN}},
        "mcp": {
            "servers": {"crm": {"transport": "openapi", "url": "http://127.0.0.1:9",
                "openapi": {"spec": spec.to_string()}, "pin_tool_definitions": true}},
            "gateways": {"g": {"servers": ["crm"]}}
        },
        "policies": {"mcp": {"servers": ["*"]}}
    })
}

struct Running {
    data: SocketAddr,
    admin: SocketAddr,
    shutdown: Box<dyn FnOnce() + Send>,
    tasks: JoinSet<()>,
}

impl Running {
    async fn start(dir: &ScratchDir, spec: &Value) -> Running {
        let file = dir.path().join("gw.json");
        std::fs::write(&file, config(dir, spec).to_string()).unwrap();
        let node = start(ServeOptions {
            config: Some(file),
            test_mode: true,
            no_config: None,
        })
        .await
        .unwrap();
        let (data, admin) = (node.data_addr().unwrap(), node.admin_addr().unwrap());
        let stop = node.rt.shutdown.clone();
        let shutdown: Box<dyn FnOnce() + Send> = Box::new(move || stop.cancel());
        let mut tasks = JoinSet::new();
        tasks.spawn(node.run());
        Running {
            data,
            admin,
            shutdown,
            tasks,
        }
    }

    async fn stop(mut self) {
        (self.shutdown)();
        while self.tasks.join_next().await.is_some() {}
    }

    async fn admin(&self, method: &str, path: &str, body: &str) -> (u16, Value) {
        let bearer = format!("Bearer {TOKEN}");
        let (status, _, text) = exchange(
            self.admin,
            method,
            path,
            &[("authorization", bearer.as_str())],
            body,
        )
        .await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn approve(&self) -> Value {
        let (status, body) = self
            .admin("POST", "/admin/v1/mcp/servers/crm:approve_tools", "{}")
            .await;
        assert_eq!(status, 200, "{body}");
        body
    }

    async fn status(&self) -> Value {
        let (status, body) = self.admin("GET", "/admin/v1/mcp/servers/crm/status", "").await;
        assert_eq!(status, 200, "{body}");
        body
    }

    async fn reset_cache(&self) {
        let (status, body) = self
            .admin("POST", "/admin/v1/test/reset", r#"{"scopes":["cache"]}"#)
            .await;
        assert_eq!(status, 204, "{body}");
    }

    /// Reads the server's tools through a gateway client, as an operator's client would, and returns
    /// the JSON-RPC answer. This is what records the observed fingerprints after a restart.
    async fn list_tools(&self, key_name: &str) -> Value {
        let body = json!({"name": key_name, "spec": {}}).to_string();
        let (status, created) = self.admin("POST", "/admin/v1/keys", &body).await;
        assert_eq!(status, 201, "{created}");
        let auth = format!("Bearer {}", created["secret"].as_str().unwrap());
        let accept = ("accept", "application/json, text/event-stream");

        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "t", "version": "1"}}});
        let (status, headers, body) = exchange(
            self.data,
            "POST",
            "/mcp/g",
            &[("authorization", auth.as_str()), accept],
            &init.to_string(),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        let session = headers
            .iter()
            .find(|(k, _)| k == "mcp-session-id")
            .map(|(_, v)| v.clone())
            .unwrap();

        let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}});
        let (status, _, body) = exchange(
            self.data,
            "POST",
            "/mcp/g",
            &[
                ("authorization", auth.as_str()),
                accept,
                ("mcp-session-id", session.as_str()),
                ("mcp-protocol-version", "2025-06-18"),
            ],
            &list.to_string(),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        serde_json::from_str(&body).unwrap_or_else(|_| panic!("not JSON: {body}"))
    }
}

fn tool<'a>(status: &'a Value, name: &str) -> &'a Value {
    status["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("{name} is not in {status}"))
}

fn listed(answer: &Value) -> Vec<&str> {
    answer["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_survives_a_restart_and_its_tools_stay_visible() {
    let dir = ScratchDir::new("mcp-approvals-restart");
    let first = Running::start(&dir, &spec("Find customers.")).await;
    let approved = first.approve().await;
    assert_eq!(approved["tools"].as_array().unwrap().len(), 2, "{approved}");
    first.stop().await;

    let second = Running::start(&dir, &spec("Find customers.")).await;
    let restored = second.status().await;
    assert!(restored["approved_at"].is_string(), "{restored}");
    assert_eq!(
        listed(&second.list_tools("after-restart").await),
        ["search_customers", "list_orders"]
    );
    let status = second.status().await;
    for name in ["search_customers", "list_orders"] {
        assert_eq!(tool(&status, name)["approved"], json!(true), "{status}");
        assert_eq!(tool(&status, name)["hidden"], json!(false), "{status}");
    }
    second.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_definition_changed_across_a_restart_hides_only_that_tool() {
    let dir = ScratchDir::new("mcp-approvals-changed");
    let first = Running::start(&dir, &spec("Find customers.")).await;
    first.approve().await;
    first.stop().await;

    let second = Running::start(&dir, &spec("Find customers, now with orders.")).await;
    assert_eq!(
        listed(&second.list_tools("after-change").await),
        ["list_orders"],
        "only the unchanged tool is listed"
    );
    let status = second.status().await;
    assert_eq!(
        tool(&status, "search_customers")["approved"],
        json!(false),
        "{status}"
    );
    assert_eq!(
        tool(&status, "search_customers")["hidden"],
        json!(true),
        "{status}"
    );
    assert_eq!(tool(&status, "list_orders")["approved"], json!(true), "{status}");
    assert_eq!(tool(&status, "list_orders")["hidden"], json!(false), "{status}");
    second.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_test_reset_forgets_stored_approvals_across_a_restart() {
    let dir = ScratchDir::new("mcp-approvals-reset");
    let first = Running::start(&dir, &spec("Find customers.")).await;
    first.approve().await;
    first.stop().await;

    let second = Running::start(&dir, &spec("Find customers.")).await;
    assert!(second.status().await["approved_at"].is_string());
    second.reset_cache().await;
    assert_eq!(second.status().await["approved_at"], Value::Null);
    second.stop().await;

    let third = Running::start(&dir, &spec("Find customers.")).await;
    assert_eq!(third.status().await["approved_at"], Value::Null);
    assert!(listed(&third.list_tools("after-reset").await).is_empty());
    let status = third.status().await;
    for name in ["search_customers", "list_orders"] {
        assert_eq!(tool(&status, name)["approved"], Value::Null, "{status}");
        assert_eq!(tool(&status, name)["hidden"], json!(true), "{status}");
    }
    third.stop().await;
}

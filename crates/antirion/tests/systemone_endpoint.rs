//! A typed decision (`POST /v1/systemone`) is routed, metered and logged like any other operation:
//! the usage the engine reported is booked, the answer is recorded, the client sees the model it
//! asked for, a deployment that never declared the operation is refused without being tried, a stream
//! is refused before any upstream call, and a chat-shaped `empty_content` policy does not read an
//! `answers` body as an empty answer.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use antirion::startup::{ServeOptions, start};
use antirion_testkit::ScratchDir;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

const TOKEN: &str = "systemone-bootstrap-token-0123456789abcdef";

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
    (status, headers, rest.to_owned())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// The decision the engine answers: one choice, with the usage the vendor wire reports.
fn answers() -> Value {
    json!({"model": "engine-v9",
        "answers": {"q": {"type": "choice", "choice": "a", "confidence": 0.91,
            "probabilities": {"a": 0.91, "b": 0.09}}},
        "usage": {"input_tokens": 44, "output_tokens": 52}})
}

/// An upstream counting the calls it was asked to answer, so a refusal can be proven to have made
/// none.
async fn upstream(listener: TcpListener, calls: Arc<AtomicUsize>, asked: Arc<Mutex<String>>) {
    let mut connections = JoinSet::new();
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let calls = Arc::clone(&calls);
        let asked = Arc::clone(&asked);
        connections.spawn(async move {
            let mut seen = Vec::new();
            let mut buf = [0u8; 4096];
            let head = loop {
                let Ok(n) = stream.read(&mut buf).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                seen.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&seen).into_owned();
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
                        break head.to_owned();
                    }
                }
            };
            let _ = calls.fetch_add(1, Ordering::SeqCst);
            // The engine is addressed by its own deployment model, never by the client's alias.
            let all = String::from_utf8_lossy(&seen).into_owned();
            if let Some((_, sent)) = all.split_once("\r\n\r\n")
                && let Ok(v) = serde_json::from_str::<Value>(sent)
                && let Some(m) = v.get("model").and_then(Value::as_str)
            {
                *asked.lock().unwrap() = m.to_owned();
            }
            let path = head.split(' ').nth(1).unwrap_or_default();
            let text = if path.ends_with("/systemone") {
                answers().to_string()
            } else {
                json!({"id": "chatcmpl-1", "object": "chat.completion", "created": 1, "model": "up-gpt",
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"},
                        "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4}})
                .to_string()
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
    }
}

struct Gateway {
    data: SocketAddr,
    admin: SocketAddr,
    calls: Arc<AtomicUsize>,
    /// The `model` the gateway put on the wire to the engine.
    asked: Arc<Mutex<String>>,
    shutdown: Box<dyn FnOnce() + Send>,
    tasks: JoinSet<()>,
    mock: JoinSet<()>,
    _dir: ScratchDir,
}

impl Gateway {
    /// A gateway with a decision engine that declares `systemone`, a plain OpenAI deployment that
    /// declares nothing, a chat Model on it, and Models `decide` (the engine), `chat-only` (the plain
    /// OpenAI deployment) and `reask` (the engine, with the chat-shaped `empty_content` policy).
    async fn start(name: &str) -> Gateway {
        let dir = ScratchDir::new(name);
        let mock = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mock_addr = mock.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let asked = Arc::new(Mutex::new(String::new()));
        let mut mock_task = JoinSet::new();
        mock_task.spawn(upstream(mock, Arc::clone(&calls), Arc::clone(&asked)));
        let config = json!({
            "version": 1,
            "server": {"listen": "127.0.0.1:0"},
            "admin": {"listen": "127.0.0.1:0"},
            "store": {"embedded": {"path": dir.path().join("data").to_string_lossy()}},
            "auth": {"admin": {"bootstrap_token": TOKEN}},
            "providers": {
                "engine": {"type": "openai_compatible", "base_url": format!("http://{mock_addr}/v1"),
                           "auth": {"method": "none"},
                           "capabilities": {"operations": ["systemone"]}},
                "plain": {"type": "openai", "base_url": format!("http://{mock_addr}/v1"),
                          "auth": {"api_key": "not-a-key"}},
            },
            "deployments": {
                "engine": {"provider": "engine", "model": "engine-current"},
                "plain": {"provider": "plain", "model": "up-gpt"},
            },
            "models": {
                "decide": {"target": {"deployment": "engine"}},
                "chat-only": {"target": {"deployment": "plain"}},
                "reask": {"target": {"deployment": "engine"},
                          "policies": {"resilience": {"retry": {"max_attempts": 3, "tier_retries": 0,
                              "retry_on_response": [{"empty_content": true}]}}}},
                "guarded": {"target": {"deployment": "engine"},
                            "policies": {"guardrails": {"output": ["secret"]}}},
            },
            "guardrails": {"definitions": {
                "secret": {"type": "regex", "action": "block",
                           "regex": {"deny": ["ACCT-[0-9]{4}"]}}
            }},
            "policies": {"observability": {"content": {"enabled": true}}}
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
        let token = node.rt.shutdown.clone();
        let shutdown: Box<dyn FnOnce() + Send> = Box::new(move || token.cancel());
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            let _ = node.run().await;
        });
        Gateway {
            data,
            admin,
            calls,
            asked,
            shutdown,
            tasks,
            mock: mock_task,
            _dir: dir,
        }
    }

    async fn stop(mut self) {
        (self.shutdown)();
        while self.tasks.join_next().await.is_some() {}
        self.mock.abort_all();
    }

    /// An Admin API call with the bootstrap token: (status, body).
    async fn admin(&self, method: &str, path: &str) -> (u16, Value) {
        let auth = format!("Bearer {TOKEN}");
        let (status, _, text) =
            exchange(self.admin, method, path, &[("authorization", auth.as_str())], "").await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// A new key with `spec`; its secret.
    async fn key(&self, name: &str, spec: Value) -> String {
        let auth = format!("Bearer {TOKEN}");
        let body = json!({"name": name, "spec": spec}).to_string();
        let (status, _, text) = exchange(
            self.admin,
            "POST",
            "/admin/v1/keys",
            &[("authorization", auth.as_str())],
            &body,
        )
        .await;
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(status, 201, "{v}");
        v["secret"].as_str().unwrap().to_owned()
    }

    /// A decision request: (status, headers, body).
    async fn decide(&self, key: &str, body: &Value) -> (u16, Vec<(String, String)>, String) {
        let bearer = format!("Bearer {key}");
        exchange(
            self.data,
            "POST",
            "/v1/systemone",
            &[("authorization", bearer.as_str())],
            &body.to_string(),
        )
        .await
    }

    /// The request-log record of `request_id`, once written.
    async fn record(&self, request_id: &str) -> Value {
        for _ in 0..100 {
            let (_, list) = self
                .admin("GET", &format!("/admin/v1/logs?request_id={request_id}"))
                .await;
            if let Some(id) = list["items"][0]["id"].as_str() {
                let (status, record) = self.admin("GET", &format!("/admin/v1/logs/{id}")).await;
                assert_eq!(status, 200, "{record}");
                return record;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("no record for {request_id}");
    }
}

fn decision(model: &str) -> Value {
    json!({"model": model, "state": "the invoice names a vendor and a due date",
        "questions": {"q": {"type": "choice", "instructions": "which team owns this?",
            "criteria": {"a": "billing", "b": "unrelated"}}}})
}

#[tokio::test(flavor = "multi_thread")]
async fn a_decision_books_the_usage_and_answers_as_the_requested_model() {
    let gw = Gateway::start("systemone-decision").await;
    let key = gw.key("k", json!({})).await;

    let (status, headers, body) = gw.decide(&key, &decision("decide")).await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(status, 200, "{body}");
    // The client sees the Model it asked for, not the engine's own name.
    assert_eq!(body["model"], "decide", "{body}");
    // The alias the client used is not what the engine is called by.
    assert_eq!(*gw.asked.lock().unwrap(), "engine-current", "{body}");
    assert_eq!(body["answers"]["q"]["choice"], "a", "{body}");

    let record = gw.record(header(&headers, "x-request-id").unwrap()).await;
    assert_eq!(record["operation"], "systemone", "{record}");
    assert_eq!(record["dialect"], "systemone", "{record}");
    // The vendor wire's `usage.input_tokens` / `output_tokens`, booked as the upstream's own.
    assert_eq!(record["usage"]["source"], "upstream", "{record}");
    assert_eq!(record["usage"]["input"], 44, "{record}");
    assert_eq!(record["usage"]["output"], 52, "{record}");
    assert_eq!(record["attempts"][0]["outcome"], "success", "{record}");
    // What a decision record does NOT carry, pinned because the gateway reads nothing inside
    // `answers`: no finish reason, no tool call, and no price until the engine is on the price list.
    assert_eq!(record["finish_reasons"], json!([]), "{record}");
    assert_eq!(record["tool_call_count"], 0, "{record}");
    assert_eq!(record["cost"]["status"], "unpriced", "{record}");
    // The answer itself is in the log, and the name the engine replied with is kept as
    // `response_model`. Three ids are in play: the alias the client asked for (`decide`), the
    // deployment id put on the wire (`engine-current`) and the id the engine answered with
    // (`engine-v9`), so no assertion here can be satisfied by copying one of the others.
    assert_eq!(record["response_model"], "engine-v9", "{record}");
    let logged = &record["content"]["response"]["body"];
    assert_eq!(logged["answers"]["q"]["choice"], "a", "{record}");
    gw.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_attached_output_guardrail_is_recorded_skipped_not_run() {
    let gw = Gateway::start("systemone-guardrail").await;
    let key = gw.key("k", json!({})).await;

    // The output guardrail stage inspects a body grammar it knows, and `answers` is not one yet
    // (`guard::dialect_of` has no decision arm). The interim contract is therefore: the decision is
    // served and the attached guardrail is recorded `skipped` — never silently passed as clean.
    // The same path covers the input stage too (`guard::uninspectable`), and the global
    // `guardrails.on_uninspectable: "block"` flips one of these skips into a refusal; this
    // gateway leaves it at its default, so the decision is served.
    let (status, headers, body) = gw.decide(&key, &decision("guarded")).await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["answers"]["q"]["choice"], "a", "{body}");

    let record = gw.record(header(&headers, "x-request-id").unwrap()).await;
    let guardrails = record["guardrails"].as_array().expect("a guardrail trail");
    assert_eq!(guardrails.len(), 1, "{record}");
    assert_eq!(guardrails[0]["name"], "secret", "{record}");
    assert_eq!(guardrails[0]["stage"], "output", "{record}");
    assert_eq!(guardrails[0]["verdict"], "skipped", "{record}");
    gw.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deployment_without_the_operation_is_refused_and_untouched() {
    let gw = Gateway::start("systemone-undeclared").await;
    let key = gw.key("k", json!({})).await;
    let (head_status, health_before) = gw.admin("GET", "/admin/v1/runtime/deployments").await;
    assert_eq!(head_status, 200, "{health_before}");
    let before = gw.calls.load(Ordering::SeqCst);

    // A `type: openai` deployment declares no operations, so no cell exists for the endpoint and the
    // decision is excluded at resolve: 400, no upstream call, and no movement in the breaker that
    // guards its chat traffic. The reason is the generic one a rerank ask gets for the same thing.
    let (status, _, body) = gw.decide(&key, &decision("chat-only")).await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["antirion"]["code"], "unsupported_capability", "{body}");
    assert_eq!(body["antirion"]["reason"], "dialects", "{body}");
    assert_eq!(body["antirion"]["stage"], "resolve", "{body}");
    assert_eq!(body["antirion"]["attempts"], 0, "{body}");
    // The message must name the operation that was asked for and the model it was refused on.
    // Asserting the whole sentence would fail on a reword, which is not a behaviour change.
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("systemone"), "must name the operation: {msg}");
    assert!(msg.contains("chat-only"), "must name the model: {msg}");
    assert_eq!(
        gw.calls.load(Ordering::SeqCst),
        before,
        "the decision reached an undeclared deployment"
    );
    let (_, health_after) = gw.admin("GET", "/admin/v1/runtime/deployments").await;
    // Pinned to the breaker, which is what this claim is about: the same rows carry
    // clock-derived fields (`open_until`, `cooldown_until`), null while nothing has tripped
    // but ticking once a deployment sees traffic, so whole-body equality fails on a re-run.
    let breaker = |body: &Value| -> Vec<Value> {
        body["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|r| {
                json!([
                    r["deployment"],
                    r["state"],
                    r["trips"],
                    r["consecutive"],
                    r["window"]
                ])
            })
            .collect()
    };
    assert_eq!(
        breaker(&health_before),
        breaker(&health_after),
        "the breaker moved on a probe"
    );

    // Chat on the same deployment is still routable: the endpoint, not the deployment, was refused.
    let bearer = format!("Bearer {key}");
    let (status, _, body) = exchange(
        gw.data,
        "POST",
        "/v1/chat/completions",
        &[("authorization", bearer.as_str())],
        &json!({"model": "chat-only", "messages": [{"role": "user", "content": "go"}]}).to_string(),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    gw.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_asked_of_a_decision_is_refused_before_any_call() {
    let gw = Gateway::start("systemone-no-stream").await;
    let key = gw.key("k", json!({})).await;
    let before = gw.calls.load(Ordering::SeqCst);

    let mut body = decision("decide");
    body["stream"] = json!(true);
    let (status, _, text) = gw.decide(&key, &body).await;
    let text: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(status, 400, "{text}");
    assert_eq!(text["antirion"]["reason"], "invalid_value", "{text}");
    assert_eq!(text["error"]["param"], "stream", "{text}");
    assert_eq!(
        gw.calls.load(Ordering::SeqCst),
        before,
        "a refused stream still called the engine"
    );
    gw.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_empty_content_policy_lets_an_answer_through() {
    let gw = Gateway::start("systemone-empty-content").await;
    let key = gw.key("k", json!({})).await;

    // `retry_on_response: [{empty_content: true}]` is written for chat. An `answers` body carries
    // no assistant text, so matching it here would fail every attempt on that model.
    let (status, headers, body) = gw.decide(&key, &decision("reask")).await;
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(status, 200, "{body}");
    let record = gw.record(header(&headers, "x-request-id").unwrap()).await;
    assert_eq!(record["attempt_count"], 1, "{record}");
    assert_eq!(record["attempts"][0]["outcome"], "success", "{record}");
    gw.stop().await;
}

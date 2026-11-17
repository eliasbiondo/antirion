//! Every audit entry the admin plane writes reads back with `changes` as the contract states it: a
//! list of `{op, path, before, after}` change objects, or null. The danger-zone actions and the
//! other actions that record facts instead of committing a generation are run, then every entry,
//! as the store keeps it and as `GET /audit` serves it, has its `changes` validated against
//! `AuditEntry` in the Admin API contract.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use antirion::startup::{ServeOptions, start};
use antirion_config::schema::UserSchema;
use antirion_config::{Format, parse};
use antirion_testkit::ScratchDir;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinSet;

const TOKEN: &str = "audit-changes-bootstrap-token-0123456789abcdef";

async fn call(admin: SocketAddr, method: &str, path: &str, content_type: &str, body: &str) -> (u16, Value) {
    let mut stream = TcpStream::connect(admin).await.unwrap();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nhost: test\r\nconnection: close\r\nauthorization: Bearer {TOKEN}\r\n\
         content-type: {content_type}\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let body = text.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

/// Every `$ref` to a component schema under `v`.
fn refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if let Some(name) = m
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| r.strip_prefix("#/components/schemas/"))
            {
                out.push(name.to_owned());
            }
            m.values().for_each(|x| refs(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| refs(x, out)),
        _ => {}
    }
}

/// `#/components/schemas/X` read as `#/$defs/X`.
fn local_refs(v: &mut Value) {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(r)) = m.get_mut("$ref")
                && let Some(name) = r.strip_prefix("#/components/schemas/")
            {
                *r = format!("#/$defs/{name}");
            }
            m.values_mut().for_each(local_refs);
        }
        Value::Array(a) => a.iter_mut().for_each(local_refs),
        _ => {}
    }
}

/// The contract's schema of `AuditEntry.changes`, with the component schemas it references as
/// `$defs`.
fn changes_schema() -> UserSchema {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../openapi/admin-api.yaml");
    let contract = parse(&std::fs::read(path).unwrap(), Format::Yaml).unwrap();
    let schemas = &contract["components"]["schemas"];
    let changes = schemas["AuditEntry"]["properties"]["changes"].clone();
    assert!(changes.is_object(), "the contract's AuditEntry has no changes");
    let mut defs = Map::new();
    let mut todo = Vec::new();
    refs(&changes, &mut todo);
    while let Some(name) = todo.pop() {
        if defs.contains_key(&name) {
            continue;
        }
        let s = schemas[&name].clone();
        assert!(s.is_object(), "the contract has no schema {name}");
        refs(&s, &mut todo);
        defs.insert(name, s);
    }
    let mut schema = changes;
    schema["$defs"] = Value::Object(defs);
    local_refs(&mut schema);
    UserSchema::compile(&schema).unwrap()
}

async fn job_done(admin: SocketAddr, id: &str) {
    for _ in 0..200 {
        let (status, job) = call(
            admin,
            "GET",
            &format!("/admin/v1/jobs/{id}"),
            "application/json",
            "",
        )
        .await;
        assert_eq!(status, 200, "{job}");
        if job["state"] != "running" {
            assert_eq!(job["state"], "succeeded", "{job}");
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the export job {id} did not finish");
}

/// Runs the audited actions and returns the audit log as `GET /audit` serves it.
async fn audited_actions(dir: &ScratchDir) -> Vec<Value> {
    let spec = json!({"openapi": "3.1.0", "info": {"title": "crm", "version": "1"},
        "paths": {"/customers": {"get": {"operationId": "search_customers", "summary": "Find customers.",
            "responses": {"200": {"description": "ok"}}}}}})
    .to_string();
    let config = json!({
        "version": 1,
        "server": {"listen": "127.0.0.1:0"},
        "admin": {"listen": "127.0.0.1:0"},
        "store": {"embedded": {"path": dir.path().join("data").to_string_lossy()}},
        "auth": {"admin": {"bootstrap_token": TOKEN}},
        "providers": {"p": {"type": "openai", "base_url": "http://127.0.0.1:9/v1", "auth": {"api_key": "sk-up"}}},
        "deployments": {"d": {"provider": "p", "model": "up-model"}},
        "models": {"m": {"target": {"deployment": "d"}}},
        "organizations": {"acme": {}},
        "mcp": {"servers": {"crm": {"transport": "openapi", "url": "http://127.0.0.1:9",
            "openapi": {"spec": spec}}}},
        "observability": {"alerts": {"webhooks": {"hook": {
            "url": "http://127.0.0.1:9/hook", "events": ["*"],
            "signing_secret": "literal-signing-secret-0123456789"
        }}}}
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
    let admin = node.admin_addr().unwrap();
    let shutdown = node.rt.shutdown.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(node.run());

    const JSON: &str = "application/json";
    const MERGE: &str = "application/merge-patch+json";
    let actions: [(&str, &str, &str, &str); 15] = [
        // The danger zone.
        (
            "POST",
            "/admin/v1/runtime/ingress:pause",
            JSON,
            r#"{"duration":"1m","reason":"drill"}"#,
        ),
        ("POST", "/admin/v1/runtime/ingress:resume", JSON, "{}"),
        (
            "POST",
            "/admin/v1/runtime/ingress:pause",
            JSON,
            r#"{"duration":"1m","reason":"drill","organization":"acme"}"#,
        ),
        (
            "POST",
            "/admin/v1/runtime/ingress:resume",
            JSON,
            r#"{"organization":"acme"}"#,
        ),
        (
            "POST",
            "/admin/v1/organizations/acme:schedule_deletion",
            JSON,
            r#"{"grace":"1h"}"#,
        ),
        ("POST", "/admin/v1/organizations/acme:cancel_deletion", JSON, "{}"),
        (
            "POST",
            "/admin/v1/settings/security:rotate_secrets",
            JSON,
            r#"{"scope":["webhook_secrets","sessions","scim_token"]}"#,
        ),
        ("GET", "/admin/v1/runtime/encryption", JSON, ""),
        // Other actions that record facts, and generation commits.
        ("PUT", "/admin/v1/routes/r", JSON, r#"{"spec":{"tree":"d"}}"#),
        (
            "PATCH",
            "/admin/v1/routes/r",
            MERGE,
            r#"{"spec":{"enabled":false}}"#,
        ),
        ("DELETE", "/admin/v1/routes/r/affinity", JSON, ""),
        ("POST", "/admin/v1/mcp/servers/crm:approve_tools", JSON, "{}"),
        ("POST", "/admin/v1/keys", JSON, r#"{"name":"k","spec":{}}"#),
        ("POST", "/admin/v1/keys/k:rotate", JSON, "{}"),
        ("GET", "/admin/v1/logs:export?format=ndjson", JSON, ""),
    ];
    for (method, path, content_type, body) in actions {
        let (status, answer) = call(admin, method, path, content_type, body).await;
        assert!((200..300).contains(&status), "{method} {path}: {status} {answer}");
    }
    let (status, job) = call(admin, "POST", "/admin/v1/organizations/acme:export", JSON, "{}").await;
    assert_eq!(status, 202, "{job}");
    let id = job["id"].as_str().unwrap().to_owned();
    job_done(admin, &id).await;
    let (status, _) = call(admin, "GET", &format!("/admin/v1/jobs/{id}/download"), JSON, "").await;
    assert_eq!(status, 200);

    let (status, page) = call(admin, "GET", "/admin/v1/audit?limit=1000", JSON, "").await;
    assert_eq!(status, 200, "{page}");

    shutdown.cancel();
    while tasks.join_next().await.is_some() {}
    page["items"].as_array().cloned().unwrap()
}

/// Every audit entry as the store keeps it, oldest first.
fn stored_entries(dir: &ScratchDir) -> Vec<Value> {
    let db = rusqlite::Connection::open(dir.path().join("data/state.db")).unwrap();
    let mut q = db.prepare("SELECT entry FROM audit ORDER BY seq").unwrap();
    q.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|e| serde_json::from_str(&e.unwrap()).unwrap())
        .collect()
}

fn check(schema: &UserSchema, entries: &[Value], form: &str) {
    for e in entries {
        if let Some((at, why)) = schema.first_violation(&e["changes"]) {
            panic!(
                "the changes of the {form} audit entry of {} break the contract at `{at}`: {why}\n{e:#}",
                e["action"]
            );
        }
    }
    let actions: Vec<&str> = entries.iter().filter_map(|e| e["action"].as_str()).collect();
    for expected in [
        "runtime.ingress.pause",
        "runtime.ingress.resume",
        "organizations.schedule_deletion",
        "organizations.cancel_deletion",
        "organizations.export",
        "jobs.download",
        "settings.security.rotate_secrets",
        "settings.security.secrets_revealed",
        "routes.update",
        "routes.affinity.delete",
        "mcp.servers.approve_tools",
        "keys.rotate",
        "logs.export",
    ] {
        assert!(
            actions.contains(&expected),
            "no {form} audit entry of {expected} among {actions:?}"
        );
    }
}

#[test]
fn the_contract_schema_of_changes_refuses_what_is_not_a_list_of_changes() {
    let schema = changes_schema();
    assert!(schema.accepts(&Value::Null));
    assert!(schema.accepts(&json!([])));
    assert!(schema.accepts(&json!([
        {"op": "update", "path": "runtime.ingress", "before": null, "after": {"was_paused": true}}
    ])));
    for bad in [
        json!({"organization": null, "was_paused": true}),
        json!(true),
        json!(["routes.r"]),
        json!([{"op": "replace", "path": "routes.r", "before": null, "after": null}]),
        json!([{"op": "update", "path": "routes.r"}]),
        json!([{"op": "update", "path": "routes.r", "before": null, "after": 3}]),
    ] {
        assert!(!schema.accepts(&bad), "{bad} is accepted");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_audit_entry_is_written_and_read_back_with_its_changes_as_a_list_of_changes() {
    let dir = ScratchDir::new("audit-changes");
    let served = audited_actions(&dir).await;
    let schema = changes_schema();
    check(&schema, &stored_entries(&dir), "stored");
    check(&schema, &served, "served");
}

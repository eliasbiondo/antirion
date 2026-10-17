//! `openapi` servers: the operations of an OpenAPI 3.0, 3.1 or 3.2 (or Swagger 2.0) document as MCP
//! tools. Each included operation with an `operationId` is a tool whose input schema holds its path,
//! query and header parameters by name and its JSON body as `body`; a call sends the HTTP request
//! through the gateway's egress and answers with the response as a text item (and, when the
//! operation declares an object response schema, as `structuredContent`). The gateway itself plays
//! the MCP server: `initialize`, `server/discover` and the lists are answered from the document.

use bytes::Bytes;
use http::header::{ACCEPT, CONTENT_TYPE};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Uri};
use http_body_util::BodyExt;
use serde_json::{Map, Value, json};

use crate::compile::{Filter, ServerRt};
use crate::http::Env;
use crate::upstream::{self, LATEST_LEGACY, MODERN, UpError};

const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];
/// Nested `$ref`s followed before a schema is cut short.
const MAX_REF_DEPTH: usize = 32;
const MAX_ANSWER: usize = 16 << 20;

/// Where a parameter goes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum In {
    Path,
    Query,
    Header,
}

#[derive(Clone, Debug)]
struct Param {
    name: String,
    at: In,
}

/// One operation exposed as a tool.
#[derive(Clone, Debug)]
pub(crate) struct Operation {
    method: Method,
    /// The path template, with `{name}` placeholders.
    path: String,
    params: Vec<Param>,
    has_body: bool,
    structured: bool,
    /// The tool definition listed to clients.
    tool: Value,
}

/// The tools of one `openapi` server.
#[derive(Clone, Debug, Default)]
pub(crate) struct OpenApi {
    pub(crate) operations: Vec<Operation>,
    /// The server version the document names.
    version: String,
}

/// Inlines local `$ref`s (`#/components/...`, `#/definitions/...`) of `v` from `doc`.
fn resolve(doc: &Value, v: &Value, depth: usize) -> Value {
    if depth > MAX_REF_DEPTH {
        return json!({});
    }
    match v {
        Value::Object(map) => {
            if let Some(target) = map.get("$ref").and_then(Value::as_str)
                && let Some(pointer) = target.strip_prefix('#')
                && let Some(found) = doc.pointer(pointer)
            {
                return resolve(doc, found, depth + 1);
            }
            Value::Object(
                map.iter()
                    .map(|(k, x)| (k.clone(), resolve(doc, x, depth + 1)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(|x| resolve(doc, x, depth + 1)).collect()),
        other => other.clone(),
    }
}

/// The JSON schema of a Swagger 2.0 or OpenAPI 3 parameter.
fn param_schema(p: &Value) -> Value {
    if let Some(s) = p.get("schema") {
        return s.clone();
    }
    let mut s = Map::new();
    for k in [
        "type", "format", "items", "enum", "default", "minimum", "maximum", "pattern",
    ] {
        if let Some(v) = p.get(k) {
            s.insert(k.to_owned(), v.clone());
        }
    }
    if let Some(d) = p.get("description") {
        s.insert("description".into(), d.clone());
    }
    Value::Object(s)
}

/// The JSON schema of a request or response `content` map (OpenAPI 3), JSON media types first.
fn content_schema(content: Option<&Value>) -> Option<Value> {
    let c = content?.as_object()?;
    c.iter()
        .find(|(k, _)| k.starts_with("application/json") || k.ends_with("+json"))
        .or_else(|| c.iter().next())
        .and_then(|(_, m)| m.get("schema").cloned())
}

fn success_schema(op: &Value, swagger: bool) -> Option<Value> {
    let responses = op.get("responses")?.as_object()?;
    let (_, r) = responses.iter().find(|(code, _)| code.starts_with('2'))?;
    if swagger {
        r.get("schema").cloned()
    } else {
        content_schema(r.get("content"))
    }
}

fn operation(
    doc: &Value,
    path: &str,
    method: &str,
    op: &Value,
    shared: &[Value],
    swagger: bool,
) -> Option<Operation> {
    let name = op.get("operationId")?.as_str()?.to_owned();
    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut params = Vec::new();
    let mut has_body = false;
    let all = shared.iter().chain(
        op.get("parameters")
            .and_then(Value::as_array)
            .into_iter()
            .flatten(),
    );
    for p in all {
        let p = resolve(doc, p, 0);
        let (Some(pname), Some(location)) = (
            p.get("name").and_then(Value::as_str),
            p.get("in").and_then(Value::as_str),
        ) else {
            continue;
        };
        let at = match location {
            "path" => In::Path,
            "query" => In::Query,
            "header" => In::Header,
            "body" if swagger => {
                has_body = true;
                properties.insert(
                    "body".into(),
                    p.get("schema").cloned().unwrap_or_else(|| json!({})),
                );
                if p.get("required").and_then(Value::as_bool) == Some(true) {
                    required.push(Value::from("body"));
                }
                continue;
            }
            _ => continue,
        };
        properties.insert(pname.to_owned(), param_schema(&p));
        if at == In::Path || p.get("required").and_then(Value::as_bool) == Some(true) {
            required.push(Value::from(pname));
        }
        params.push(Param {
            name: pname.to_owned(),
            at,
        });
    }
    if !swagger && let Some(rb) = op.get("requestBody") {
        let rb = resolve(doc, rb, 0);
        has_body = true;
        properties.insert(
            "body".into(),
            content_schema(rb.get("content")).unwrap_or_else(|| json!({})),
        );
        if rb.get("required").and_then(Value::as_bool) == Some(true) {
            required.push(Value::from("body"));
        }
    }
    let mut input = json!({"type": "object", "properties": properties});
    if !required.is_empty()
        && let Some(obj) = input.as_object_mut()
    {
        obj.insert("required".into(), Value::Array(required));
    }
    let output = success_schema(op, swagger)
        .map(|s| resolve(doc, &s, 0))
        .filter(|s| s.get("type").and_then(Value::as_str) == Some("object"));
    let mut tool = json!({"name": name, "inputSchema": input});
    let description = op
        .get("description")
        .or_else(|| op.get("summary"))
        .and_then(Value::as_str);
    if let (Some(d), Some(obj)) = (description, tool.as_object_mut()) {
        obj.insert("description".into(), Value::from(d));
    }
    if let (Some(o), Some(obj)) = (&output, tool.as_object_mut()) {
        obj.insert("outputSchema".into(), o.clone());
    }
    Some(Operation {
        method: method.to_ascii_uppercase().parse().ok()?,
        path: path.to_owned(),
        params,
        has_body,
        structured: output.is_some(),
        tool,
    })
}

impl OpenApi {
    /// Reads an OpenAPI or Swagger document (JSON or YAML text); `None` when it is neither.
    pub(crate) fn parse(text: &str, operations: &Filter) -> Option<OpenApi> {
        let format = if text.trim_start().starts_with('{') {
            antirion_config::Format::Json
        } else {
            antirion_config::Format::Yaml
        };
        let doc = antirion_config::parse(text.as_bytes(), format).ok()?;
        let swagger = doc.get("swagger").is_some();
        if !swagger && doc.get("openapi").is_none() {
            return None;
        }
        let base_path = if swagger {
            doc.get("basePath")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim_end_matches('/')
                .to_owned()
        } else {
            String::new()
        };
        let mut out = OpenApi {
            operations: Vec::new(),
            version: doc
                .pointer("/info/version")
                .and_then(Value::as_str)
                .unwrap_or("1.0.0")
                .to_owned(),
        };
        for (path, item) in doc.get("paths").and_then(Value::as_object).into_iter().flatten() {
            let shared: Vec<Value> = item
                .get("parameters")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for method in METHODS {
                let Some(op) = item.get(method) else { continue };
                let full = format!("{base_path}{path}");
                if let Some(o) = operation(&doc, &full, method, op, &shared, swagger) {
                    let id = o.tool.get("name").and_then(Value::as_str).unwrap_or_default();
                    if operations.admits(id) {
                        out.operations.push(o);
                    }
                }
            }
        }
        Some(out)
    }

    fn tools(&self) -> Vec<Value> {
        self.operations.iter().map(|o| o.tool.clone()).collect()
    }

    /// The answer the gateway gives, as this server, to `msg`.
    pub(crate) async fn answer(&self, env: &Env, server: &ServerRt, msg: &Value) -> Result<Value, UpError> {
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
        let params = msg.get("params");
        let result = match method {
            "initialize" => {
                let asked = params
                    .and_then(|p| p.get("protocolVersion"))
                    .and_then(Value::as_str)
                    .filter(|v| upstream::is_legacy(v))
                    .unwrap_or(LATEST_LEGACY);
                json!({"protocolVersion": asked, "capabilities": {"tools": {"listChanged": false}},
                       "serverInfo": {"name": server.name, "version": self.version}})
            }
            "server/discover" => json!({
                "supportedVersions": upstream::supported(),
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": server.name, "version": self.version},
            }),
            "ping" => json!({}),
            "tools/list" => json!({"tools": self.tools()}),
            "prompts/list" => json!({"prompts": []}),
            "resources/list" => json!({"resources": []}),
            "resources/templates/list" => json!({"resourceTemplates": []}),
            "tools/call" => return self.call(env, server, &id, params).await,
            other => {
                return Ok(json!({"jsonrpc": "2.0", "id": id,
                                 "error": {"code": -32601, "message": format!("Method not found: {other}")}}));
            }
        };
        let mut result = result;
        if msg
            .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
            .and_then(Value::as_str)
            == Some(MODERN)
            && let Some(obj) = result.as_object_mut()
        {
            obj.insert("resultType".into(), Value::from("complete"));
        }
        Ok(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    async fn call(
        &self,
        env: &Env,
        server: &ServerRt,
        id: &Value,
        params: Option<&Value>,
    ) -> Result<Value, UpError> {
        let name = params
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(op) = self
            .operations
            .iter()
            .find(|o| o.tool.get("name").and_then(Value::as_str) == Some(name))
        else {
            return Ok(json!({"jsonrpc": "2.0", "id": id,
                             "error": {"code": -32602, "message": format!("Unknown tool: {name}")}}));
        };
        let empty = Map::new();
        let args = params
            .and_then(|p| p.get("arguments"))
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let (uri, headers, body) = op.request(server, args)?;
        let mut headers = headers;
        for (k, v) in &server.headers {
            headers.insert(k.clone(), v.clone());
        }
        if let Some(auth) = &server.auth {
            let value = auth
                .authorization(&env.egress, env.egress.subject.as_deref(), server.init_timeout)
                .await?;
            headers.insert(http::header::AUTHORIZATION, value);
        }
        let resp = env
            .egress
            .send(op.method.clone(), uri, headers, body, server.request_timeout)
            .await
            .map_err(|_| UpError::Connect)?;
        let status = resp.status;
        let json_answer = resp
            .headers
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.contains("json"));
        let bytes = tokio::time::timeout(server.request_timeout, resp.body.collect())
            .await
            .map_err(|_| UpError::Timeout)?
            .map_err(|_| UpError::Reset)?
            .to_bytes();
        if bytes.len() > MAX_ANSWER {
            return Err(UpError::NotMcp);
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let result = if status.is_success() {
            let mut r = json!({"content": [{"type": "text", "text": text}], "isError": false});
            if op.structured
                && json_answer
                && let Ok(v @ Value::Object(_)) = serde_json::from_slice::<Value>(&bytes)
                && let Some(obj) = r.as_object_mut()
            {
                obj.insert("structuredContent".into(), v);
            }
            r
        } else {
            json!({"content": [{"type": "text", "text": format!("HTTP {}: {text}", status.as_u16())}],
                   "isError": true})
        };
        Ok(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }
}

/// Percent-encodes everything outside the RFC 3986 unreserved set.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A parameter value as text: strings as they are, other values as their JSON.
fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl Operation {
    /// The HTTP request of a call with `args`.
    fn request(
        &self,
        server: &ServerRt,
        args: &Map<String, Value>,
    ) -> Result<(Uri, HeaderMap, Bytes), UpError> {
        let mut path = self.path.clone();
        let mut query = Vec::new();
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        for p in &self.params {
            let Some(v) = args.get(&p.name) else { continue };
            match p.at {
                In::Path => path = path.replace(&format!("{{{}}}", p.name), &encode(&text_of(v))),
                In::Query => match v {
                    Value::Array(items) => {
                        for i in items {
                            query.push(format!("{}={}", encode(&p.name), encode(&text_of(i))));
                        }
                    }
                    other => query.push(format!("{}={}", encode(&p.name), encode(&text_of(other)))),
                },
                In::Header => {
                    if let (Ok(n), Ok(val)) = (
                        HeaderName::from_bytes(p.name.as_bytes()),
                        HeaderValue::from_str(&text_of(v)),
                    ) {
                        headers.insert(n, val);
                    }
                }
            }
        }
        let body = match args.get("body").filter(|_| self.has_body) {
            Some(b) => {
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                Bytes::from(serde_json::to_vec(b).map_err(|_| UpError::Rejected)?)
            }
            None => Bytes::new(),
        };
        let base = server.url.to_string();
        let mut url = format!("{}{path}", base.trim_end_matches('/'));
        if !query.is_empty() {
            url.push(if url.contains('?') { '&' } else { '?' });
            url.push_str(&query.join("&"));
        }
        let uri = url.parse().map_err(|_| UpError::Rejected)?;
        Ok((uri, headers, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r##"
openapi: 3.1.0
info: {title: users, version: 2.0.0}
paths:
  /users/{id}:
    parameters:
      - {name: id, in: path, required: true, schema: {type: integer}}
    get:
      operationId: getUser
      summary: A user.
      parameters:
        - {name: q, in: query, schema: {type: string}}
        - {name: X-Trace, in: header, schema: {type: string}}
      responses:
        "200":
          content:
            application/json:
              schema: {$ref: "#/components/schemas/User"}
  /users:
    post:
      operationId: createUser
      requestBody:
        required: true
        content:
          application/json:
            schema: {$ref: "#/components/schemas/User"}
      responses: {"201": {description: created}}
    delete:
      operationId: dropAll
      responses: {"204": {description: gone}}
components:
  schemas:
    User: {type: object, properties: {name: {type: string}}}
"##;

    fn all() -> Filter {
        Filter {
            include: vec!["*".into()],
            exclude: vec!["drop*".into()],
        }
    }

    #[test]
    fn operations_become_tools() {
        let api = OpenApi::parse(DOC, &all()).unwrap();
        let names: Vec<&str> = api
            .operations
            .iter()
            .map(|o| o.tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["getUser", "createUser"]);
        let get = &api.operations[0];
        assert_eq!(get.tool["inputSchema"]["required"], json!(["id"]));
        assert_eq!(
            get.tool["inputSchema"]["properties"]["q"],
            json!({"type": "string"})
        );
        assert_eq!(get.tool["outputSchema"]["type"], "object");
        assert!(get.structured);
        let create = &api.operations[1];
        assert_eq!(create.tool["inputSchema"]["required"], json!(["body"]));
        assert_eq!(create.tool["inputSchema"]["properties"]["body"]["type"], "object");
        assert!(create.tool.get("outputSchema").is_none());
    }

    #[test]
    fn swagger_documents_and_garbage() {
        let swagger = r#"{"swagger": "2.0", "basePath": "/v1", "paths": {"/pets": {"get": {"operationId": "listPets",
            "parameters": [{"name": "limit", "in": "query", "type": "integer"}],
            "responses": {"200": {"schema": {"type": "object"}}}}}}}"#;
        let api = OpenApi::parse(swagger, &all()).unwrap();
        assert_eq!(api.operations[0].path, "/v1/pets");
        assert_eq!(
            api.operations[0].tool["inputSchema"]["properties"]["limit"],
            json!({"type": "integer"})
        );
        assert!(OpenApi::parse("{\"not\": \"openapi\"}", &all()).is_none());
    }
}

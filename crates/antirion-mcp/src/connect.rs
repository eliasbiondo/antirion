//! Opening a legacy upstream session (`initialize`, then `notifications/initialized`), and building the
//! links of an existing session or of a modern request.

use std::sync::Arc;

use http::HeaderMap;
use serde_json::{Value, json};

use crate::compile::{Protocol, ServerRt, Transport};
use crate::http::{Env, Req};
use crate::link::Link;
use crate::select::upstream_headers;
use crate::ssetransport;
use crate::token::{SessionClaims, Upstream};
use crate::upstream::{self, Era, MODERN, UpError};

/// The `clientInfo` the gateway sends when it has none from the client.
pub(crate) fn gateway_client_info() -> Value {
    json!({"name": "antirion", "version": env!("CARGO_PKG_VERSION")})
}

/// Initializes `server` at `version` for a request.
pub(crate) async fn open(
    req: &Req,
    server: &Arc<ServerRt>,
    version: &str,
    capabilities: &Value,
    client_info: &Value,
    keep: bool,
) -> Result<(Link, Value), UpError> {
    let headers = upstream_headers(req, server);
    open_with(
        &req.env,
        server,
        headers,
        version,
        capabilities,
        client_info,
        keep,
    )
    .await
}

/// Initializes `server` at `version` with `headers`. With `keep`, an HTTP+SSE stream stays open for
/// the session.
pub(crate) async fn open_with(
    env: &Env,
    server: &Arc<ServerRt>,
    headers: HeaderMap,
    version: &str,
    capabilities: &Value,
    client_info: &Value,
    keep: bool,
) -> Result<(Link, Value), UpError> {
    let opened = Box::pin(initialize_with(
        env,
        server,
        headers,
        version,
        capabilities,
        client_info,
        keep,
    ))
    .await;
    env.state.record_contact(
        &server.name,
        env.now,
        opened.as_ref().err().map(UpError::describe),
    );
    opened
}

async fn initialize_with(
    env: &Env,
    server: &Arc<ServerRt>,
    headers: HeaderMap,
    version: &str,
    capabilities: &Value,
    client_info: &Value,
    keep: bool,
) -> Result<(Link, Value), UpError> {
    if server.protocol == Protocol::Modern {
        return crate::bridge::modern_initialize(env, server, headers, version, capabilities, client_info)
            .await;
    }
    let mut link = Link {
        server: Arc::clone(server),
        era: Era::Legacy,
        pv: version.to_owned(),
        sid: None,
        sse: None,
        headers,
        meta: None,
    };
    if server.transport == Transport::Sse {
        let conn =
            ssetransport::connect(&env.egress, server, &link.headers, &env.tasks, &env.shutdown).await?;
        link.sid = Some(conn.endpoint.to_string());
        link.sse = Some(conn);
    }
    let params = json!({"protocolVersion": version, "capabilities": capabilities, "clientInfo": client_info});
    let answered = link.initialize(env, params).await;
    let version = answered.as_ref().ok().and_then(|msg| {
        msg.get("result")?
            .get("protocolVersion")?
            .as_str()
            .filter(|v| upstream::is_legacy(v))
            .map(str::to_owned)
    });
    let (msg, version) = match (answered, version) {
        (Ok(msg), Some(v)) => (msg, v),
        (Err(e), _) => {
            link.close(env).await;
            return Err(e);
        }
        (Ok(_), None) => {
            link.close(env).await;
            return Err(UpError::Rejected);
        }
    };
    link.pv = version;
    let _ = link
        .post(
            env,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;
    if keep && let (Some(sid), Some(conn)) = (&link.sid, &link.sse) {
        env.state.sse.lock().insert(sid.clone(), Arc::clone(conn));
    }
    let result = msg.get("result").cloned().unwrap_or(Value::Null);
    Ok((link, result))
}

/// The link of one server of an existing session; a modern-only server is reached in the modern era
/// with the session's client capabilities and info.
pub(crate) fn session_link(req: &Req, server: &Arc<ServerRt>, up: &Upstream, claims: &SessionClaims) -> Link {
    if server.protocol == Protocol::Modern {
        return Link {
            server: Arc::clone(server),
            era: Era::Modern,
            pv: up.pv.clone(),
            sid: None,
            sse: None,
            headers: upstream_headers(req, server),
            meta: Some(crate::bridge::session_meta(&claims.caps, &claims.ci)),
        };
    }
    let sse = if server.transport == Transport::Sse {
        up.sid
            .as_ref()
            .and_then(|sid| req.env.state.sse.lock().get(sid).cloned())
            .filter(|c| !c.is_closed())
    } else {
        None
    };
    Link {
        server: Arc::clone(server),
        era: Era::Legacy,
        pv: up.pv.clone(),
        sid: up.sid.clone(),
        sse,
        headers: upstream_headers(req, server),
        meta: None,
    }
}

/// The link of one server for a modern request carrying `meta`.
pub(crate) fn modern_link(req: &Req, server: &Arc<ServerRt>, meta: &Value) -> Link {
    Link {
        server: Arc::clone(server),
        era: Era::Modern,
        pv: MODERN.to_owned(),
        sid: None,
        sse: None,
        headers: upstream_headers(req, server),
        meta: Some(meta.clone()),
    }
}

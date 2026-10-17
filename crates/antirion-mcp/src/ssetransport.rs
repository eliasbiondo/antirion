//! Upstream servers on the 2024-11-05 HTTP+SSE transport: one long-lived GET stream per upstream
//! session, held by the node that opened it, and POSTs to the endpoint the stream announced.
//! Answers are routed to the waiting request by id; other messages go to the only request in flight,
//! or to the client's server-to-client stream.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, Method, Uri};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::compile::ServerRt;
use crate::egress::Egress;
use crate::sse::{ReadError, Reader};
use crate::upstream::{Answer, Call, Era, Source, UpError, send_once};

/// One open HTTP+SSE session.
#[derive(Debug)]
pub(crate) struct SseConn {
    pub(crate) endpoint: Uri,
    waiters: Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>,
    listeners: Mutex<Vec<mpsc::UnboundedSender<Value>>>,
    stop: CancellationToken,
}

impl SseConn {
    fn dispatch(&self, msg: Value) {
        let is_response = msg.get("result").is_some() || msg.get("error").is_some();
        if is_response {
            let key = msg.get("id").map(Value::to_string).unwrap_or_default();
            if let Some(tx) = self.waiters.lock().remove(&key) {
                let _ = tx.send(msg);
            }
            return;
        }
        let only = {
            let waiters = self.waiters.lock();
            (waiters.len() == 1)
                .then(|| waiters.values().next().cloned())
                .flatten()
        };
        if let Some(tx) = only
            && tx.send(msg.clone()).is_ok()
        {
            return;
        }
        self.listeners.lock().retain(|l| l.send(msg.clone()).is_ok());
    }

    /// Messages that belong to no request, for a client's server-to-client stream.
    pub(crate) fn listen(&self) -> mpsc::UnboundedReceiver<Value> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.listeners.lock().push(tx);
        rx
    }

    pub(crate) fn close(&self) {
        self.stop.cancel();
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.stop.is_cancelled()
    }
}

/// Opens the GET stream, waits for the `endpoint` event (skipping comments and other events), and
/// keeps reading on a tracked task until the session is closed or the node shuts down.
pub(crate) async fn connect(
    egress: &Egress,
    server: &ServerRt,
    headers: &HeaderMap,
    tasks: &TaskTracker,
    shutdown: &CancellationToken,
) -> Result<Arc<SseConn>, UpError> {
    let call = Call {
        server,
        era: Era::Legacy,
        version: None,
        session: None,
        headers,
        timeout: server.init_timeout,
    };
    let reply = send_once(egress, &call, Method::GET, server.url.clone(), None).await?;
    let Answer::Stream(Source::Sse(mut reader)) = reply.answer else {
        return Err(UpError::NotMcp);
    };
    let deadline = tokio::time::Instant::now() + server.init_timeout;
    let endpoint = loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match reader.next(left).await {
            Ok(ev) if ev.event.as_deref() == Some("endpoint") => break ev.data,
            Ok(_) => {}
            Err(ReadError::Idle) => return Err(UpError::Timeout),
            Err(ReadError::Ended) => return Err(UpError::NotMcp),
        }
    };
    let endpoint = resolve(&server.url, endpoint.trim()).ok_or(UpError::NotMcp)?;
    let conn = Arc::new(SseConn {
        endpoint,
        waiters: Mutex::new(HashMap::new()),
        listeners: Mutex::new(Vec::new()),
        stop: shutdown.child_token(),
    });
    tasks.spawn(pump(reader, Arc::clone(&conn)));
    Ok(conn)
}

async fn pump(mut reader: Reader, conn: Arc<SseConn>) {
    const IDLE: Duration = Duration::from_secs(3600);
    loop {
        tokio::select! {
            () = conn.stop.cancelled() => break,
            ev = reader.next(IDLE) => match ev {
                Ok(ev) => {
                    if ev.event.as_deref().is_some_and(|e| e != "message") {
                        continue;
                    }
                    if let Ok(msg) = serde_json::from_str::<Value>(&ev.data) {
                        conn.dispatch(msg);
                    }
                }
                Err(ReadError::Idle) => {}
                Err(ReadError::Ended) => break,
            }
        }
    }
    conn.stop.cancel();
    conn.waiters.lock().clear();
    conn.listeners.lock().clear();
}

fn resolve(base: &Uri, endpoint: &str) -> Option<Uri> {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        return endpoint.parse().ok();
    }
    let scheme = base.scheme_str()?;
    let authority = base.authority()?;
    let path = if endpoint.starts_with('/') {
        endpoint.to_owned()
    } else {
        let dir = base.path().rsplit_once('/').map_or("", |(d, _)| d);
        format!("{dir}/{endpoint}")
    };
    format!("{scheme}://{authority}{path}").parse().ok()
}

/// Posts a request and returns the channel its answer (and any message sent while it runs) arrives on.
pub(crate) async fn request(
    egress: &Egress,
    conn: &SseConn,
    server: &ServerRt,
    headers: &HeaderMap,
    msg: &Value,
) -> Result<Source, UpError> {
    if conn.is_closed() {
        return Err(UpError::SessionGone);
    }
    let key = msg.get("id").map(Value::to_string).unwrap_or_default();
    let (tx, rx) = mpsc::unbounded_channel();
    conn.waiters.lock().insert(key.clone(), tx);
    if let Err(e) = post(egress, conn, server, headers, msg).await {
        conn.waiters.lock().remove(&key);
        return Err(e);
    }
    Ok(Source::Channel(rx))
}

/// Posts a notification or a response.
pub(crate) async fn post(
    egress: &Egress,
    conn: &SseConn,
    server: &ServerRt,
    headers: &HeaderMap,
    msg: &Value,
) -> Result<(), UpError> {
    let call = Call {
        server,
        era: Era::Legacy,
        version: None,
        session: None,
        headers,
        timeout: server.request_timeout,
    };
    let body = Bytes::from(serde_json::to_vec(msg).unwrap_or_default());
    match send_once(egress, &call, Method::POST, conn.endpoint.clone(), Some(body)).await {
        Ok(_) => Ok(()),
        Err(UpError::Status(404)) => Err(UpError::SessionGone),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_resolution() {
        let base: Uri = "http://127.0.0.1:9/srv/sse".parse().unwrap();
        assert_eq!(
            resolve(&base, "/srv/messages?sessionId=1").unwrap().to_string(),
            "http://127.0.0.1:9/srv/messages?sessionId=1"
        );
        assert_eq!(
            resolve(&base, "messages?x=1").unwrap().to_string(),
            "http://127.0.0.1:9/srv/messages?x=1"
        );
        assert_eq!(
            resolve(&base, "http://other:1/m").unwrap().to_string(),
            "http://other:1/m"
        );
    }

    #[test]
    fn answers_go_to_their_waiter_and_others_to_the_only_request() {
        let conn = SseConn {
            endpoint: Uri::from_static("http://h/m"),
            waiters: Mutex::new(HashMap::new()),
            listeners: Mutex::new(Vec::new()),
            stop: CancellationToken::new(),
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        conn.waiters.lock().insert("7".into(), tx);
        let mut listener = conn.listen();
        conn.dispatch(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/progress"}));
        conn.dispatch(serde_json::json!({"jsonrpc": "2.0", "id": 7, "result": {}}));
        assert_eq!(rx.try_recv().unwrap()["method"], "notifications/progress");
        assert_eq!(rx.try_recv().unwrap()["id"], 7);
        conn.dispatch(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/message"}));
        assert_eq!(listener.try_recv().unwrap()["method"], "notifications/message");
    }
}

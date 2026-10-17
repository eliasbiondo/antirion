//! The listeners of a cluster node that waits for its store at startup: they answer the probes
//! (alive, not ready) and refuse every other request until the node can build its runtime.

use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;

use antirion_core::{RequestId, Stage};
use antirion_error::{Code, GatewayError, Message};
use antirion_runtime::probes::Waiting;
use axum::body::Body;
use http::header::{ALLOW, CONTENT_TYPE};
use http::{HeaderValue, Method, Response};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;

use crate::tls::Plane;
use crate::{Bound, HEADER_READ_TIMEOUT, ListenerTls, builder, handshake};

impl Bound {
    /// Answers on both listeners while `until` runs, then closes the connections it accepted and
    /// returns `until`'s output; the listeners stay bound for [`crate::serve`].
    pub async fn answer_while<F: Future>(&self, waiting: &Waiting, tls: &ListenerTls, until: F) -> F::Output {
        let waiting = Arc::new(waiting.clone());
        let mut conns = JoinSet::new();
        tokio::pin!(until);
        loop {
            tokio::select! {
                out = &mut until => {
                    conns.abort_all();
                    while conns.join_next().await.is_some() {}
                    return out;
                }
                Ok((stream, _)) = self.data.accept() => {
                    let acceptor = tls.data.as_ref().map(|t| t.acceptor());
                    conns.spawn(answer(stream, Plane::Data, acceptor, Arc::clone(&waiting)));
                }
                Ok((stream, _)) = self.admin.accept() => {
                    let acceptor = tls.admin.as_ref().map(|t| t.acceptor());
                    conns.spawn(answer(stream, Plane::Admin, acceptor, Arc::clone(&waiting)));
                }
                Some(_) = conns.join_next(), if !conns.is_empty() => {}
            }
        }
    }
}

async fn answer(stream: TcpStream, plane: Plane, acceptor: Option<Arc<TlsAcceptor>>, waiting: Arc<Waiting>) {
    let _ = stream.set_nodelay(true);
    let service = service_fn(move |req: http::Request<hyper::body::Incoming>| {
        let resp = match plane {
            Plane::Data => data_answer(&waiting, req.method(), req.uri().path()),
            Plane::Admin => antirion_admin::waiting(&waiting, req.method(), req.uri().path()),
        };
        async move { Ok::<_, Infallible>(resp) }
    });
    let result = match acceptor {
        None => builder().serve_connection(TokioIo::new(stream), service).await,
        Some(acceptor) => {
            let Some(s) = handshake(&acceptor, stream, HEADER_READ_TIMEOUT).await else {
                return;
            };
            builder().serve_connection(TokioIo::new(s), service).await
        }
    };
    if let Err(e) = result {
        tracing::debug!(error = %e, "a connection to a node waiting for its store ended with an error");
    }
}

/// The data listener's answer while the node waits for its store: the probes, and 503
/// `store_unavailable` for every other request.
fn data_answer(waiting: &Waiting, method: &Method, path: &str) -> Response<Body> {
    let live = match path {
        "/-/livez" => true,
        "/-/readyz" => false,
        _ => {
            let mut e = GatewayError::with_reason(
                antirion_pipeline::reasons::STORE_UNAVAILABLE,
                Stage::Accept,
                Message::Fixed("The node is waiting for its store."),
            );
            e.retry_after = Some(std::time::Duration::from_secs(1));
            return error(path, &e);
        }
    };
    if method != Method::GET && method != Method::HEAD {
        let e = GatewayError::new(
            Code::MethodNotAllowed,
            Stage::Accept,
            Message::Fixed("Probes accept GET and HEAD only."),
        )
        .header(ALLOW, HeaderValue::from_static("GET, HEAD"));
        return error(path, &e);
    }
    let (status, body) = waiting.probe(live);
    let bytes = if method == Method::HEAD {
        Vec::new()
    } else {
        serde_json::to_vec(&body).unwrap_or_default()
    };
    let mut resp = Response::new(Body::from(bytes));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}

fn error(path: &str, e: &GatewayError) -> Response<Body> {
    let envelope = antirion_dialects::envelope_for_path(path);
    let rendered = antirion_error::render(e, envelope, &RequestId::generate(), None);
    let mut resp = Response::new(Body::from(rendered.body));
    *resp.status_mut() = rendered.status;
    *resp.headers_mut() = rendered.headers;
    resp
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use http::StatusCode;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    fn waiting() -> Waiting {
        Waiting {
            node_id: "n1".into(),
            version: "1.0.0".into(),
        }
    }

    async fn get(addr: SocketAddr, path: &str) -> String {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let req = format!("GET {path} HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }

    #[test]
    fn the_data_listener_refuses_requests_with_the_store_reason() {
        let r = data_answer(&waiting(), &Method::POST, "/v1/chat/completions");
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(r.headers().get(http::header::RETRY_AFTER).unwrap(), "1");
        let r = data_answer(&waiting(), &Method::DELETE, "/-/readyz");
        assert_eq!(r.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn the_listeners_answer_probes_while_the_node_waits_and_stay_bound() {
        let local: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let bound = Bound::bind(local, local).await.unwrap();
        let (data, admin) = (bound.data_addr().unwrap(), bound.admin_addr().unwrap());
        let tls = ListenerTls {
            data: None,
            admin: None,
        };
        let node = waiting();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let probes = async {
            let live = get(data, "/-/livez").await;
            let ready = get(data, "/-/readyz").await;
            let admin_ready = get(admin, "/-/readyz").await;
            let _ = release.send(());
            (live, ready, admin_ready)
        };
        let (out, (live, ready, admin_ready)) = tokio::join!(
            bound.answer_while(&node, &tls, async { released.await.is_ok() }),
            probes
        );
        assert!(out);
        assert!(live.starts_with("HTTP/1.1 200"), "{live}");
        assert!(
            ready.starts_with("HTTP/1.1 503") && ready.contains("\"not_ready\""),
            "{ready}"
        );
        assert!(ready.contains("\"degraded\":[\"store\"]"), "{ready}");
        assert!(admin_ready.starts_with("HTTP/1.1 503"), "{admin_ready}");
        assert_eq!(bound.data_addr(), Some(data), "the listener is kept for serving");
    }
}

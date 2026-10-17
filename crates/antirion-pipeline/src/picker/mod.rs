//! The external endpoint picker of a pool (`picker: external`): before the request goes to a model
//! server, one gRPC stream to the picker carries the request headers (with the pool's endpoints as
//! the subset hint) and the whole body; the picker answers with the destination, a list of
//! `ip:port` endpoints of the pool. The stream stays open while the request is served, and after
//! the final attempt the gateway sends the response headers with the endpoint that produced the
//! terminal response, then ends the stream.

mod proto;

use std::sync::Arc;
use std::time::Duration;

use antirion_core::headers as h;
use antirion_egress::{HttpVersion, InboundBody, OutboundRequest, Payload, Tail, TailError, TrustClass};
use antirion_router::pickers::Asked;
use antirion_router::pool::Picker;
use antirion_router::state::RoutingState;
use antirion_router::tree::Node;
use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Uri};
use http_body_util::BodyExt;
use tokio::sync::mpsc;

use crate::ctx::Ctx;
use crate::infer::Request;
use crate::parse::Parsed;

const PROCESS: &str = "/envoy.service.ext_proc.v3.ExternalProcessor/Process";
/// The largest answer read from a picker.
const MAX_ANSWER: usize = 1 << 20;

type Sender = mpsc::UnboundedSender<Result<Bytes, TailError>>;

/// The open stream of one pick, kept to report the served endpoint.
pub(crate) struct Session {
    tx: Sender,
    body: InboundBody,
    timeout: Duration,
    /// The pool's leaves and their endpoints, to name the one that served.
    endpoints: Vec<(usize, String)>,
    /// Where route state learns which deployment served: the routing state, the pool's path and
    /// its leaves' deployments.
    log: (Arc<RoutingState>, String, Vec<(usize, String)>),
}

/// The `host:port` of an `http(s)` URL, the port filled in from the scheme.
pub(crate) fn endpoint_of(url: &str) -> Option<String> {
    let uri: Uri = url.parse().ok()?;
    let host = uri.host()?;
    let port = uri.port_u16().or_else(|| match uri.scheme_str() {
        Some("https") => Some(443),
        Some("http") => Some(80),
        _ => None,
    })?;
    Some(format!("{host}:{port}"))
}

/// The endpoint of each child of a pool node, by position.
fn pool_endpoints(
    cx: &Ctx,
    route: &antirion_router::tree::CompiledRoute,
    node: &Node,
) -> Vec<(usize, usize, String)> {
    let providers = &cx.snap.providers;
    node.children
        .iter()
        .enumerate()
        .filter_map(|(pos, &child)| {
            let provider = match &route.node(child)?.kind {
                antirion_router::NodeKind::Deployment(d) => {
                    Arc::clone(&providers.deployments.get(d)?.provider)
                }
                antirion_router::NodeKind::ProviderModel { provider, .. } => {
                    Arc::clone(providers.providers.get(provider)?)
                }
                _ => return None,
            };
            Some((pos, child, endpoint_of(provider.base_url.as_deref()?)?))
        })
        .collect()
}

/// The positions of the pool's children a destination names, in its order: an `ip:port` names
/// that endpoint, a bare IP every endpoint of that IP.
fn chosen(destination: &[String], endpoints: &[(usize, usize, String)]) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for d in destination {
        let bare = !d.contains(':') || (d.starts_with('[') && d.ends_with(']'));
        for (pos, _, e) in endpoints {
            let hit = if bare {
                e.rsplit_once(':').is_some_and(|(host, _)| host == d)
            } else {
                e == d
            };
            if hit && !out.contains(pos) {
                out.push(*pos);
            }
        }
    }
    out
}

/// The request headers the picker sees: the pseudo-headers and the client's, without credentials
/// or the gateway's own.
fn request_headers(req: &Request<'_>) -> Vec<(String, Vec<u8>)> {
    let uri = &req.parts.uri;
    let mut out = vec![
        (
            ":method".to_owned(),
            req.parts.method.as_str().as_bytes().to_vec(),
        ),
        (
            ":path".to_owned(),
            uri.path_and_query()
                .map_or("/", |p| p.as_str())
                .as_bytes()
                .to_vec(),
        ),
    ];
    if let Some(a) = req.parts.headers.get(http::header::HOST) {
        out.push((":authority".to_owned(), a.as_bytes().to_vec()));
    }
    for (k, v) in &req.parts.headers {
        if !antirion_egress::headers::always_denied(k.as_str()) {
            out.push((k.as_str().to_owned(), v.as_bytes().to_vec()));
        }
    }
    out
}

fn raw_body(parsed: &Parsed) -> Bytes {
    match parsed {
        Parsed::Json { raw, .. } | Parsed::Multipart { raw, .. } => raw.clone(),
    }
}

/// Opens the stream and sends the request; the stream and its answer body.
async fn open(cx: &Ctx, picker: &Picker, first: Vec<Bytes>) -> Result<(Sender, InboundBody), String> {
    let uri: Uri = format!("{}{PROCESS}", picker.url.trim_end_matches('/'))
        .parse()
        .map_err(|_| "the picker URL is not valid".to_owned())?;
    let mut tls = cx
        .snap
        .security
        .egress_tls
        .clone()
        .ok_or_else(|| "no egress TLS profile".to_owned())?;
    tls.http = if uri.scheme_str() == Some("https") {
        HttpVersion::H2
    } else {
        HttpVersion::H2c
    };
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/grpc"));
    headers.insert(http::header::TE, HeaderValue::from_static("trailers"));
    if let Ok(v) = HeaderValue::from_str(cx.request_id.as_str()) {
        headers.insert(HeaderName::from_static(h::X_REQUEST_ID), v);
    }
    let (tx, rx) = mpsc::unbounded_channel::<Result<Bytes, TailError>>();
    let rest = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x, rx)) });
    let sent = cx
        .rt
        .egress
        .send(OutboundRequest {
            method: Method::POST,
            uri,
            headers,
            body: Payload::from_parts(first).with_tail(Tail::new(Box::pin(rest), None)),
            response_timeout: picker.timeout,
            connect_timeout: Some(picker.timeout),
            fresh_connection: false,
            trust: TrustClass::Operator,
            tls,
            policy: Arc::clone(&cx.snap.security.egress),
            resend_when_closed: false,
        })
        .await
        .map_err(|e| e.to_string())?;
    let status = sent.headers.get("grpc-status").and_then(|v| v.to_str().ok());
    if !sent.status.is_success() || status.is_some_and(|s| s != "0") {
        return Err(format!("the picker answered status {}", sent.status));
    }
    Ok((tx, sent.body))
}

/// Reads answers until the picker has answered the request body (or ended the request); the
/// destination it set, empty for no endpoint.
async fn destination(body: &mut InboundBody) -> Result<Vec<String>, String> {
    let mut frames = proto::Frames::default();
    let mut read = 0usize;
    let mut found: Option<String> = None;
    loop {
        while let Some(msg) = frames.next() {
            let msg = msg.map_err(|()| "the picker sent a compressed message".to_owned())?;
            let a = proto::answer(&msg).ok_or_else(|| "the picker's answer is malformed".to_owned())?;
            if a.immediate {
                return Ok(Vec::new());
            }
            if a.destination.is_some() {
                found = a.destination;
            }
            if a.body {
                let d = found.ok_or_else(|| "the picker set no destination".to_owned())?;
                return Ok(proto::entries(&d));
            }
        }
        let frame = body
            .frame()
            .await
            .ok_or_else(|| "the picker ended the stream without a destination".to_owned())?
            .map_err(|e| e.to_string())?;
        if let Ok(data) = frame.into_data() {
            read += data.len();
            if read > MAX_ANSWER {
                return Err("the picker's answer is too large".to_owned());
            }
            frames.push(&data);
        }
    }
}

/// The deployment a pool leaf names (its segment for a provider model).
fn leaf_name(route: &antirion_router::tree::CompiledRoute, leaf: usize) -> Option<String> {
    let n = route.node(leaf)?;
    match &n.kind {
        antirion_router::NodeKind::Deployment(d) => Some(d.clone()),
        _ => n.segment.clone(),
    }
}

/// Asks the picker of pool `node`: the positions of the children it chose, in its order (empty for
/// no endpoint), and the open stream. The answer, or the failure, is recorded for route state.
pub(crate) async fn pick(
    cx: &Ctx,
    req: &Request<'_>,
    route: &antirion_router::tree::CompiledRoute,
    node: &Node,
    picker: &Picker,
) -> Result<(Vec<usize>, Session), String> {
    let asked = ask(cx, req, route, node, picker).await;
    let outcome = match &asked {
        Ok((positions, _)) => Asked::Picked(
            positions
                .iter()
                .filter_map(|p| node.children.get(*p))
                .filter_map(|c| leaf_name(route, *c))
                .collect(),
        ),
        Err(why) => Asked::Failed(why.clone()),
    };
    cx.rt
        .state
        .routing
        .pickers
        .asked(&node.path, cx.rt.now(), outcome);
    asked
}

async fn ask(
    cx: &Ctx,
    req: &Request<'_>,
    route: &antirion_router::tree::CompiledRoute,
    node: &Node,
    picker: &Picker,
) -> Result<(Vec<usize>, Session), String> {
    let endpoints = pool_endpoints(cx, route, node);
    let mut subset: Vec<String> = Vec::new();
    for (_, _, e) in &endpoints {
        if !subset.contains(e) {
            subset.push(e.clone());
        }
    }
    let first = vec![
        proto::frame(&proto::request_headers(&request_headers(req), &subset)),
        proto::frame(&proto::request_body(&raw_body(req.parsed))),
    ];
    let asked = async {
        let (tx, mut body) = open(cx, picker, first).await?;
        let d = destination(&mut body).await?;
        Ok::<_, String>((d, tx, body))
    };
    let (d, tx, body) = tokio::time::timeout(picker.timeout, asked)
        .await
        .map_err(|_| "the picker timed out".to_owned())??;
    let positions = chosen(&d, &endpoints);
    let names = endpoints
        .iter()
        .filter_map(|(_, leaf, _)| leaf_name(route, *leaf).map(|n| (*leaf, n)))
        .collect();
    let session = Session {
        tx,
        body,
        timeout: picker.timeout,
        endpoints: endpoints.into_iter().map(|(_, leaf, e)| (leaf, e)).collect(),
        log: (Arc::clone(&cx.rt.state.routing), node.path.clone(), names),
    };
    Ok((positions, session))
}

impl Session {
    /// Reports the leaf that produced the terminal response (none when no model server answered)
    /// and ends the stream, waiting at most the picker's timeout for it to close.
    pub(crate) async fn finish(self, served: Option<usize>) {
        let Session {
            tx,
            mut body,
            timeout,
            endpoints,
            log: (routing, path, names),
        } = self;
        if let Some((_, d)) = served.and_then(|leaf| names.iter().find(|(l, _)| *l == leaf)) {
            routing.pickers.served(&path, d);
        }
        if let Some(e) = served.and_then(|leaf| endpoints.iter().find(|(l, _)| *l == leaf)) {
            let headers = [(proto::SERVED.to_owned(), e.1.as_bytes().to_vec())];
            let _ = tx.send(Ok(proto::frame(&proto::response_headers(&headers))));
        }
        drop(tx);
        let drained = async { while let Some(Ok(_)) = body.frame().await {} };
        let _ = tokio::time::timeout(timeout, drained).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_of_urls() {
        assert_eq!(
            endpoint_of("http://10.0.0.1:8000/v1").as_deref(),
            Some("10.0.0.1:8000")
        );
        assert_eq!(
            endpoint_of("https://10.0.0.1/v1").as_deref(),
            Some("10.0.0.1:443")
        );
        assert_eq!(endpoint_of("http://host/v1").as_deref(), Some("host:80"));
        assert_eq!(endpoint_of("not a url"), None);
    }

    #[test]
    fn a_destination_names_endpoints_in_its_order() {
        let pool = vec![
            (0, 10, "10.0.0.1:8000".to_owned()),
            (1, 11, "10.0.0.2:8000".to_owned()),
            (2, 12, "10.0.0.2:9000".to_owned()),
        ];
        assert_eq!(
            chosen(&["10.0.0.2:9000".into(), "10.0.0.1:8000".into()], &pool),
            vec![2, 0]
        );
        assert_eq!(
            chosen(&["10.0.0.2".into()], &pool),
            vec![1, 2],
            "a bare IP allows any port"
        );
        assert!(chosen(&["10.0.0.9:8000".into()], &pool).is_empty());
        assert!(chosen(&[], &pool).is_empty());
    }
}

//! The data plane reached in process: one HTTP/1.1 exchange over an in-memory pipe, served by the
//! same `DataPlane` the listener serves, so a playground request takes every stage a client's
//! request takes. The peer is loopback unless the caller names the connection it acts for.

use std::convert::Infallible;
use std::sync::{Arc, Weak};

use antirion_core::Peer;
use antirion_pipeline::DataPlane;
use antirion_runtime::Runtime;
use antirion_runtime::playground::{LocalPlane, PlaneError, PlaneFuture};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

/// Buffer of the in-memory pipe, each way.
const PIPE: usize = 64 * 1024;

#[derive(Debug)]
pub(crate) struct LocalData {
    pub(crate) rt: Weak<Runtime>,
}

impl LocalPlane for LocalData {
    fn send_from(&self, req: http::Request<Bytes>, peer: Peer) -> PlaneFuture {
        let rt = self.rt.upgrade();
        Box::pin(async move {
            let rt = rt.ok_or(PlaneError::Unavailable)?;
            exchange(rt, req, peer).await
        })
    }
}

async fn exchange(
    rt: Arc<Runtime>,
    req: http::Request<Bytes>,
    peer: Peer,
) -> Result<http::Response<antirion_runtime::playground::PlaneBody>, PlaneError> {
    let (client, server) = tokio::io::duplex(PIPE);
    let data = DataPlane::new(Arc::clone(&rt));
    let service = service_fn(move |mut req: http::Request<hyper::body::Incoming>| {
        let data = data.clone();
        req.extensions_mut().insert(peer.clone());
        async move { Ok::<_, Infallible>(data.serve(req).await) }
    });
    rt.tasks.spawn(async move {
        if let Err(e) = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(server), service)
            .await
        {
            tracing::debug!(error = %e, "a local data-plane exchange ended with an error");
        }
    });
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(client))
        .await
        .map_err(|e| PlaneError::Exchange(e.to_string()))?;
    rt.tasks.spawn(async move {
        let _ = conn.await;
    });
    let resp = sender
        .send_request(req.map(Full::new))
        .await
        .map_err(|e| PlaneError::Exchange(e.to_string()))?;
    Ok(resp.map(|b| {
        b.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            .boxed()
    }))
}

//! A request written to a pooled connection the upstream had already closed (it answers with a
//! reset) is sent once more on a new connection, within the same call; one the upstream read and
//! then dropped is not.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, Method};
use http_body_util::BodyExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::{EgressClient, EgressPolicy, OutboundRequest, TlsProfile, TrustClass};

/// How the first connection treats its second request.
#[derive(Copy, Clone, Debug)]
enum Ending {
    /// Closes it after reading the request (a clean end of stream).
    Close,
    /// Resets it after reading the request.
    Reset,
    /// Resets it before the request arrives, while it sits idle in the pool.
    ResetIdle,
}

async fn read_head(sock: &mut TcpStream) -> bool {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match sock.read(&mut buf).await {
            Ok(0) | Err(_) => return false,
            Ok(n) => head.extend_from_slice(buf.get(..n).unwrap_or_default()),
        }
    }
    true
}

async fn answer(sock: &mut TcpStream) -> bool {
    sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
        .await
        .is_ok()
}

/// The first connection answers one request and then ends as `ending` says; later connections
/// answer every request.
async fn upstream(listener: TcpListener, accepted: Arc<AtomicUsize>, ending: Ending) {
    let mut conns = tokio::task::JoinSet::new();
    while let Ok((mut sock, _)) = listener.accept().await {
        let first = accepted.fetch_add(1, Ordering::SeqCst) == 0;
        conns.spawn(async move {
            if !first {
                while read_head(&mut sock).await && answer(&mut sock).await {}
                return;
            }
            if !(read_head(&mut sock).await && answer(&mut sock).await) {
                return;
            }
            match ending {
                Ending::ResetIdle => {}
                Ending::Close | Ending::Reset => {
                    if !read_head(&mut sock).await {
                        return;
                    }
                }
            }
            if matches!(ending, Ending::Reset | Ending::ResetIdle) {
                let _ = sock.set_zero_linger();
            }
            drop(sock);
        });
    }
}

fn get(addr: std::net::SocketAddr) -> OutboundRequest {
    OutboundRequest {
        method: Method::GET,
        uri: format!("http://{addr}/").parse().unwrap(),
        headers: HeaderMap::new(),
        body: Bytes::new().into(),
        response_timeout: Duration::from_secs(5),
        connect_timeout: None,
        fresh_connection: false,
        trust: TrustClass::Operator,
        tls: TlsProfile::default_profile().unwrap(),
        policy: Arc::new(EgressPolicy::default()),
        resend_when_closed: true,
    }
}

async fn second_request(ending: Ending) -> (bool, usize) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let mut server = tokio::task::JoinSet::new();
    server.spawn(upstream(listener, Arc::clone(&accepted), ending));
    let client = EgressClient::new(Duration::from_secs(2));
    let first = client.send(get(addr)).await.unwrap();
    assert_eq!(first.status, 200);
    // Reading the body to its end hands the connection back to the pool, which happens on the pool's
    // own task: wait for it, or the second request may open a new connection instead of reusing it.
    first.body.collect().await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let second = client.send(get(addr)).await;
    server.abort_all();
    (second.is_ok(), accepted.load(Ordering::SeqCst))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reused_connection_reset_before_the_response_is_resent_once() {
    for ending in [Ending::Reset, Ending::ResetIdle] {
        assert_eq!(second_request(ending).await, (true, 2), "{ending:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_the_upstream_read_and_dropped_is_not_resent() {
    assert_eq!(second_request(Ending::Close).await, (false, 1));
}

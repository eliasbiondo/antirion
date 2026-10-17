//! Egress through a forward proxy (`security.egress.proxy`). The gateway resolves and validates the
//! destination itself, then asks the proxy for a tunnel to the validated address
//! (`CONNECT <ip>:<port>`), so the proxy can never resolve the name to another address; TLS (SNI)
//! and `Host` still carry the name. Destinations that `no_proxy` names connect directly, under the
//! same rules.

use std::fmt;
use std::net::{IpAddr, SocketAddr};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ipnet::IpNet;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::connect::ConnectFailure;

/// The longest proxy answer head read.
const MAX_HEAD: usize = 8 * 1024;

/// One `no_proxy` entry.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NoProxy {
    /// `*`: every destination.
    All,
    /// A host name, lowercase.
    Host(String),
    /// `.example.com` or `*.example.com`: the subdomains of a domain.
    Suffix(String),
    /// An address or a CIDR range.
    Net(IpNet),
}

impl NoProxy {
    /// Reads one entry; `None` for an empty one.
    #[must_use]
    pub fn parse(entry: &str) -> Option<NoProxy> {
        let e = entry.trim().to_ascii_lowercase();
        if e.is_empty() {
            return None;
        }
        if e == "*" {
            return Some(NoProxy::All);
        }
        if let Ok(net) = e.parse::<IpNet>() {
            return Some(NoProxy::Net(net));
        }
        if let Ok(ip) = e.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() {
            return Some(NoProxy::Net(IpNet::from(ip)));
        }
        match e.strip_prefix("*.").or_else(|| e.strip_prefix('.')) {
            Some(domain) => Some(NoProxy::Suffix(domain.to_owned())),
            None => Some(NoProxy::Host(e)),
        }
    }

    fn matches(&self, host: &str, addrs: &[IpAddr]) -> bool {
        match self {
            NoProxy::All => true,
            NoProxy::Host(h) => h == host,
            NoProxy::Suffix(d) => host
                .strip_suffix(d.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.ends_with('.')),
            NoProxy::Net(n) => addrs.iter().any(|a| n.contains(a)),
        }
    }
}

/// The compiled proxy settings.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Proxy {
    pub host: String,
    pub port: u16,
    /// `Proxy-Authorization` from the URL's user information.
    authorization: Option<String>,
    pub no_proxy: Vec<NoProxy>,
}

impl fmt::Debug for Proxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Proxy")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("no_proxy", &self.no_proxy)
            .finish_non_exhaustive()
    }
}

impl Proxy {
    /// Reads an `http://[user:password@]host[:port]` proxy URL (port 80 by default).
    pub fn parse(url: &str, no_proxy: Vec<NoProxy>) -> Result<Proxy, &'static str> {
        let rest = url
            .strip_prefix("http://")
            .ok_or("the proxy URL must be http:// (the tunnel carries the TLS to the destination)")?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let (userinfo, hostport) = match authority.rsplit_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, authority),
        };
        let (host, port) = match hostport.strip_prefix('[') {
            Some(v6) => {
                let (host, after) = v6.split_once(']').ok_or("the proxy host is not valid")?;
                match after.strip_prefix(':') {
                    Some(p) => (host, p.parse::<u16>().map_err(|_| "the proxy port is not valid")?),
                    None => (host, 80),
                }
            }
            None => match hostport.rsplit_once(':') {
                Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "the proxy port is not valid")?),
                None => (hostport, 80),
            },
        };
        if host.is_empty() {
            return Err("the proxy URL has no host");
        }
        let authorization = userinfo.map(|u| format!("Basic {}", STANDARD.encode(percent_decode(u))));
        Ok(Proxy {
            host: host.to_owned(),
            port,
            authorization,
            no_proxy,
        })
    }

    /// Whether a destination connects directly: its name or one of its validated addresses is in
    /// `no_proxy`.
    #[must_use]
    pub fn bypassed(&self, host: &str, addrs: &[IpAddr]) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.no_proxy.iter().any(|n| n.matches(&host, addrs))
    }
}

fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = |j: usize| bytes.get(j).and_then(|c| char::from(*c).to_digit(16));
        match (b, hex(i + 1), hex(i + 2)) {
            (b'%', Some(hi), Some(lo)) => {
                out.push(u8::try_from(hi * 16 + lo).unwrap_or(b'?'));
                i += 3;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// The `CONNECT` request for a validated address.
fn connect_request(proxy: &Proxy, target: SocketAddr) -> String {
    let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if let Some(a) = &proxy.authorization {
        req.push_str("Proxy-Authorization: ");
        req.push_str(a);
        req.push_str("\r\n");
    }
    req.push_str("\r\n");
    req
}

/// The status of a proxy's answer head, once its blank line has arrived.
fn status_of(head: &[u8]) -> Option<u16> {
    let line = head.split(|b| *b == b'\n').next()?;
    let text = std::str::from_utf8(line).ok()?;
    let mut parts = text.split_whitespace();
    parts.next().filter(|v| v.starts_with("HTTP/1."))?;
    parts.next()?.parse().ok()
}

/// A connection to the proxy tunnelled to `target`.
pub(crate) async fn tunnel(proxy: &Proxy, target: SocketAddr) -> Result<TcpStream, ConnectFailure> {
    let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port))
        .await
        .map_err(|e| ConnectFailure::Io(e.kind()))?;
    let _ = stream.set_nodelay(true);
    stream
        .write_all(connect_request(proxy, target).as_bytes())
        .await
        .map_err(|e| ConnectFailure::Io(e.kind()))?;
    let mut head = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD {
            return Err(ConnectFailure::Proxy(0));
        }
        let n = stream
            .read(&mut byte)
            .await
            .map_err(|e| ConnectFailure::Io(e.kind()))?;
        if n == 0 {
            return Err(ConnectFailure::Proxy(0));
        }
        head.extend_from_slice(&byte);
    }
    match status_of(&head) {
        Some(s) if (200..300).contains(&s) => Ok(stream),
        s => Err(ConnectFailure::Proxy(s.unwrap_or(0))),
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncBufReadExt;
    use tokio::net::TcpListener;

    use super::*;

    fn no_proxy(list: &[&str]) -> Vec<NoProxy> {
        list.iter().filter_map(|e| NoProxy::parse(e)).collect()
    }

    #[test]
    fn urls_and_bypass_rules() {
        let p = Proxy::parse(
            "http://user:p%40ss@proxy.internal:3128",
            no_proxy(&["internal.example", ".corp.example", "10.0.0.0/8", "::1"]),
        )
        .unwrap();
        assert_eq!((p.host.as_str(), p.port), ("proxy.internal", 3128));
        assert_eq!(p.authorization.as_deref(), Some("Basic dXNlcjpwQHNz"));
        assert!(!format!("{p:?}").contains("Basic"));
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(p.bypassed("internal.example", &[ip("203.0.113.5")]));
        assert!(p.bypassed("a.corp.example", &[]));
        assert!(p.bypassed("corp.example", &[]));
        assert!(!p.bypassed("evilcorp.example", &[]));
        assert!(p.bypassed("anything", &[ip("10.1.2.3")]));
        assert!(p.bypassed("v6", &[ip("::1")]));
        assert!(!p.bypassed("api.openai.com", &[ip("203.0.113.5")]));
        assert_eq!(Proxy::parse("http://[::1]", Vec::new()).unwrap().port, 80);
        assert!(Proxy::parse("https://proxy:443", Vec::new()).is_err());
        assert!(Proxy::parse("http://:80", Vec::new()).is_err());
        assert!(
            Proxy::parse("http://p", no_proxy(&["*"]))
                .unwrap()
                .bypassed("x", &[])
        );
    }

    #[test]
    fn the_connect_request_names_the_address() {
        let p = Proxy::parse("http://proxy:3128", Vec::new()).unwrap();
        let v4: SocketAddr = "203.0.113.5:443".parse().unwrap();
        assert_eq!(
            connect_request(&p, v4),
            "CONNECT 203.0.113.5:443 HTTP/1.1\r\nHost: 203.0.113.5:443\r\n\r\n"
        );
        let v6: SocketAddr = "[2001:db8::1]:443".parse().unwrap();
        assert!(connect_request(&p, v6).starts_with("CONNECT [2001:db8::1]:443 HTTP/1.1"));
        assert_eq!(
            status_of(b"HTTP/1.1 200 Connection established\r\n\r\n"),
            Some(200)
        );
        assert_eq!(status_of(b"garbage\r\n\r\n"), None);
    }

    #[tokio::test]
    async fn a_tunnel_is_opened_and_a_refusal_is_a_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let tasks = tokio_util::task::TaskTracker::new();
        tasks.spawn(async move {
            for answer in ["HTTP/1.1 200 OK\r\n\r\n", "HTTP/1.1 403 Forbidden\r\n\r\n"] {
                let (sock, _) = listener.accept().await.unwrap();
                let mut sock = tokio::io::BufReader::new(sock);
                let mut line = String::new();
                sock.read_line(&mut line).await.unwrap();
                assert!(line.starts_with("CONNECT 192.0.2.7:8443 "));
                while line != "\r\n" {
                    line.clear();
                    sock.read_line(&mut line).await.unwrap();
                }
                sock.get_mut().write_all(answer.as_bytes()).await.unwrap();
            }
        });
        let p = Proxy::parse(&format!("http://127.0.0.1:{port}"), Vec::new()).unwrap();
        let target: SocketAddr = "192.0.2.7:8443".parse().unwrap();
        assert!(tunnel(&p, target).await.is_ok());
        assert!(matches!(
            tunnel(&p, target).await,
            Err(ConnectFailure::Proxy(403))
        ));
        tasks.close();
        tasks.wait().await;
    }
}

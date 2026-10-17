//! The client address of an admin request: the TCP peer, or the address a trusted proxy's
//! forwarding header names (`security.client_ip`), as the data plane resolves it. Sessions,
//! throttles and the audit log all read this value.

use antirion_core::Peer;
use antirion_runtime::Runtime;
use antirion_runtime::client_ip::ClientIpRt;
use http::request::Parts;

/// The resolved client address of a request, or `None` when the listener attached no peer.
pub(crate) fn client_addr(rt: &Runtime, parts: &Parts) -> Option<String> {
    resolved(&rt.gens.load().security.client_ip, parts)
}

fn resolved(client_ip: &ClientIpRt, parts: &Parts) -> Option<String> {
    let peer = parts.extensions.get::<Peer>()?;
    Some(client_ip.resolve(peer.addr.ip(), &parts.headers).to_string())
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use antirion_runtime::client_ip::IpHeader;

    use super::*;

    fn parts(peer: Option<&str>) -> Parts {
        let mut req = http::Request::builder()
            .header("x-forwarded-for", "198.51.100.7, 10.0.0.2")
            .body(())
            .unwrap();
        if let Some(addr) = peer {
            req.extensions_mut().insert(Peer {
                addr: addr.parse::<SocketAddr>().unwrap(),
                tls: false,
                client_cert: None,
            });
        }
        req.into_parts().0
    }

    #[test]
    fn forwarding_headers_count_only_from_trusted_proxies() {
        let untrusted = ClientIpRt::default();
        assert_eq!(
            resolved(&untrusted, &parts(Some("127.0.0.1:9"))).as_deref(),
            Some("127.0.0.1")
        );
        let trusted = ClientIpRt {
            trusted: vec!["127.0.0.0/8".parse().unwrap(), "10.0.0.0/8".parse().unwrap()],
            header: IpHeader::XForwardedFor,
        };
        assert_eq!(
            resolved(&trusted, &parts(Some("127.0.0.1:9"))).as_deref(),
            Some("198.51.100.7")
        );
        assert_eq!(resolved(&trusted, &parts(None)), None);
    }
}

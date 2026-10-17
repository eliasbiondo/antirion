//! The one place the data plane learns a request's client IP: the TCP peer the listener attached,
//! or, when that peer is a trusted proxy, the address its forwarding header names
//! (`security.client_ip`). IP allow-lists, limits and logs all read this value.

use std::net::IpAddr;

use antirion_core::Peer;
use antirion_runtime::security::SecurityRt;
use http::request::Parts;

/// The client IP of a request, or `None` when the listener attached no peer.
pub(crate) fn client_ip(security: &SecurityRt, parts: &Parts) -> Option<IpAddr> {
    let peer = parts.extensions.get::<Peer>()?;
    Some(security.client_ip.resolve(peer.addr.ip(), &parts.headers))
}

//! The data listeners. On Linux each data thread accepts on its own listener of one `SO_REUSEPORT`
//! group, among which the kernel spreads new connections, so a connection is accepted on the thread
//! that serves it. Elsewhere `SO_REUSEPORT` does not spread connections, and one listener hands its
//! connections to the data threads.

use std::io;
use std::net::SocketAddr;

use tokio::net::{TcpListener, TcpSocket};

/// Whether every data thread accepts on a listener of its own.
pub(crate) const PER_THREAD: bool = cfg!(target_os = "linux");

/// The backlog `TcpListener::bind` gives a listener.
const BACKLOG: u32 = 1024;

/// A listener on `addr` registered with the calling runtime; with `shared` on Linux, a member of
/// the `SO_REUSEPORT` group on that address.
pub(crate) fn listener(addr: SocketAddr, shared: bool) -> io::Result<TcpListener> {
    let socket = if addr.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    socket.set_reuseaddr(true)?;
    #[cfg(target_os = "linux")]
    if shared {
        socket.set_reuseport(true)?;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = shared;
    socket.bind(addr)?;
    socket.listen(BACKLOG)
}

/// A data listener on its way to the runtime it accepts on.
#[derive(Debug)]
pub(crate) enum DataListener {
    /// Registered with the runtime it accepts on.
    Ready(TcpListener),
    /// The bound listener, registered where it accepts.
    Moved(std::net::TcpListener),
    /// A further member of the group on this address, bound where it accepts.
    Joined(SocketAddr),
}

impl DataListener {
    /// The listener, registered with the calling runtime.
    pub(crate) fn open(self) -> io::Result<TcpListener> {
        match self {
            DataListener::Ready(l) => Ok(l),
            DataListener::Moved(l) => TcpListener::from_std(l),
            DataListener::Joined(addr) => listener(addr, true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_group_members_share_the_address_and_only_on_linux() {
        let first = listener("127.0.0.1:0".parse().unwrap(), PER_THREAD).unwrap();
        let addr = first.local_addr().unwrap();
        assert_eq!(DataListener::Joined(addr).open().is_ok(), PER_THREAD);
        assert!(
            listener(addr, false).is_err(),
            "an outsider cannot bind the address"
        );
    }
}

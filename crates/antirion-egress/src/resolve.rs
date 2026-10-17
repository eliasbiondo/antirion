//! Name resolution for every egress lookup: the system resolver, or the DNS servers an operator
//! names in `server.upstream.dns.servers`.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use hickory_resolver::TokioResolver;
use hickory_resolver::config::{ConnectionConfig, NameServerConfig, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;

/// How destination names are resolved.
#[derive(Clone, Default)]
pub enum Resolver {
    #[default]
    System,
    Servers(Arc<TokioResolver>),
}

impl std::fmt::Debug for Resolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Resolver::System => f.write_str("Resolver::System"),
            Resolver::Servers(_) => f.write_str("Resolver::Servers"),
        }
    }
}

/// Why a resolver could not be built.
#[derive(Debug, thiserror::Error)]
#[error("the DNS resolver could not be built")]
pub struct ResolverError;

impl Resolver {
    /// A resolver that asks only `servers` (UDP, then TCP).
    pub fn servers(servers: &[SocketAddr]) -> Result<Resolver, ResolverError> {
        let name_servers = servers
            .iter()
            .map(|s| {
                let mut udp = ConnectionConfig::udp();
                udp.port = s.port();
                let mut tcp = ConnectionConfig::tcp();
                tcp.port = s.port();
                NameServerConfig::new(s.ip(), true, vec![udp, tcp])
            })
            .collect();
        let config = ResolverConfig::from_parts(None, Vec::new(), name_servers);
        let r = TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
            .build()
            .map_err(|_| ResolverError)?;
        Ok(Resolver::Servers(Arc::new(r)))
    }

    /// The addresses of `host`.
    pub(crate) async fn lookup(&self, host: &str, port: u16) -> Option<Vec<SocketAddr>> {
        match self {
            Resolver::System => tokio::net::lookup_host((host, port))
                .await
                .ok()
                .map(Iterator::collect),
            Resolver::Servers(r) => {
                let ips = r.lookup_ip(host).await.ok()?;
                Some(ips.iter().map(|ip: IpAddr| SocketAddr::new(ip, port)).collect())
            }
        }
    }
}

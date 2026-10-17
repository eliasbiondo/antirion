//! The gateway of a cell: rendered from its profile, started on the topology's nodes (or applied to
//! running external nodes), probed until ready, and stopped afterwards.

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use antirion_bench_corpus::{SEED, entries};
use antirion_bench_loadgen::fetch;
use antirion_bench_mock::Profile;
use antirion_bench_report::model::CellDef;
use anyhow::{Context, bail};
use bytes::Bytes;
use http::Request;
use http_body_util::Full;
use serde_json::Value;

use crate::config::{Launch, NodeAddr};
use crate::ctx::Ctx;
use crate::hosts::{self, Host, Pid, Running};
use crate::procs::{self, Spawn};
use crate::profile::{self, Endpoints, NodeStatics, Rendered};

/// A running gateway.
#[derive(Debug)]
pub(crate) struct Gateway {
    pub(crate) data: Vec<SocketAddr>,
    pub(crate) admin: Vec<SocketAddr>,
    pub(crate) pids: Vec<Pid>,
    procs: Vec<Running>,
    pub(crate) rendered: Rendered,
    /// Process start to the last node's ready probe.
    pub(crate) ready: Duration,
}

pub(crate) const READY: &str = "/-/readyz";
const READY_LIMIT: Duration = Duration::from_secs(600);

impl Ctx {
    pub(crate) fn endpoints(&self, mock: Profile, hanging: bool) -> Endpoints {
        let root = format!("http://{}", self.mock_addr);
        Endpoints {
            mock: format!("{root}/{}", mock.name()),
            sink: if hanging {
                format!("{root}/__bench/hang")
            } else {
                format!("{root}/__bench/null")
            },
        }
    }

    pub(crate) fn render(&self, def: &CellDef, profile: &str) -> anyhow::Result<Rendered> {
        let mock: Profile = def.mock_profile.as_deref().unwrap_or("L0").parse()?;
        profile::render(
            &self.paths,
            profile,
            &self.endpoints(mock, def.hanging_exporters),
            SEED,
        )
    }

    /// Starts (or configures) the gateway for `def` with `profile`; `tag` names its files.
    pub(crate) async fn start_gateway(
        &self,
        def: &CellDef,
        profile: &str,
        tag: &str,
    ) -> anyhow::Result<Gateway> {
        let rendered = self.render(def, profile)?;
        let count = if rendered.cluster {
            def.nodes.max(3)
        } else {
            def.nodes.max(1)
        };
        let nodes = self.topo.nodes(def.machine.as_deref(), count)?;
        match self.topo.gateway.launch {
            Launch::Local => self.start_local(rendered, &nodes, tag).await,
            Launch::External => self.apply_external(rendered, &nodes).await,
        }
    }

    async fn start_local(
        &self,
        rendered: Rendered,
        nodes: &[NodeAddr],
        tag: &str,
    ) -> anyhow::Result<Gateway> {
        if nodes.iter().any(|n| n.agent.is_none()) && !self.gateway_bin.is_file() {
            bail!(
                "no gateway binary at {} (build it, or pass --gateway-bin)",
                self.gateway_bin.display()
            );
        }
        let configs = self.out.join("configs");
        std::fs::create_dir_all(&configs)?;
        let mut procs = Vec::new();
        let started = Instant::now();
        let result = async {
            for (i, node) in nodes.iter().enumerate() {
                let host = Host::of(node.agent);
                let data_dir = hosts::work_dir(host, &self.work)
                    .await?
                    .join(format!("{tag}-node{}", i + 1));
                let statics = NodeStatics {
                    node: *node,
                    index: i,
                    data_dir: data_dir.join("store"),
                    admin_token: &self.admin_token,
                    store: self.topo.store.as_ref(),
                };
                let doc = serde_json::to_string(&profile::startup_doc(&rendered, &statics)?)?;
                std::fs::write(configs.join(format!("{tag}-node{}.json", i + 1)), &doc)?;
                let file = data_dir.join("antirion.json");
                let spawn = Spawn {
                    name: "gateway",
                    program: self.gateway_bin.clone(),
                    args: vec!["serve".into(), "--config".into(), file.display().to_string()],
                    nice: self.topo.gateway.nice,
                    cpus: self.topo.gateway.cpus.as_deref(),
                    env: Some(&rendered.env),
                    add_env: &[],
                    cwd: None,
                    log: data_dir.join("gateway.log"),
                };
                let p = hosts::spawn(host, &spawn, std::slice::from_ref(&data_dir), &[(file, doc)]).await?;
                self.record_command(p.argv());
                procs.push(p);
            }
            for (p, node) in procs.iter_mut().zip(nodes) {
                procs::wait_ok(node.data, READY, READY_LIMIT, Some(p)).await?;
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(e) = result {
            for p in procs {
                p.stop(Duration::from_secs(5)).await;
            }
            return Err(e);
        }
        Ok(Gateway {
            data: nodes.iter().map(|n| n.data).collect(),
            admin: nodes.iter().map(|n| n.admin).collect(),
            pids: procs.iter().map(Running::pid).collect(),
            ready: started.elapsed(),
            procs,
            rendered,
        })
    }

    fn external_token(&self) -> anyhow::Result<String> {
        let var = self
            .topo
            .gateway
            .admin_token_env
            .as_deref()
            .unwrap_or("ANTIRION_BENCH_ADMIN_TOKEN");
        std::env::var(var).with_context(|| format!("set {var} to the external gateway's Admin API token"))
    }

    async fn apply_external(&self, rendered: Rendered, nodes: &[NodeAddr]) -> anyhow::Result<Gateway> {
        let token = self.external_token()?;
        let first = nodes.first().context("no node")?;
        let started = Instant::now();
        let body = serde_json::to_vec(&rendered.doc)?;
        let req = Request::post("/admin/v1/config:apply?source=bench&prune=true&wait=5s")
            .header("host", first.admin.to_string())
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body)))?;
        let (status, _, text) = fetch(first.admin, req, Duration::from_secs(120)).await?;
        if !status.is_success() {
            bail!(
                "applying {} failed: {status} {}",
                rendered.name,
                String::from_utf8_lossy(&text)
            );
        }
        for n in nodes {
            procs::wait_ok(n.data, READY, READY_LIMIT, None).await?;
        }
        Ok(Gateway {
            data: nodes.iter().map(|n| n.data).collect(),
            admin: nodes.iter().map(|n| n.admin).collect(),
            pids: Vec::new(),
            procs: Vec::new(),
            ready: started.elapsed(),
            rendered,
        })
    }

    pub(crate) async fn stop_gateway(&self, gw: Gateway) {
        for p in gw.procs {
            p.stop(Duration::from_secs(20)).await;
        }
    }

    pub(crate) fn admin_token_for(&self) -> anyhow::Result<String> {
        match self.topo.gateway.launch {
            Launch::Local => Ok(self.admin_token.clone()),
            Launch::External => self.external_token(),
        }
    }
}

async fn post(
    addr: SocketAddr,
    path: &str,
    headers: &[(&str, String)],
    body: String,
) -> anyhow::Result<(u16, http::HeaderMap, Bytes)> {
    let mut b = Request::post(path)
        .header("host", addr.to_string())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    for (k, v) in headers {
        b = b.header(*k, v);
    }
    let (s, h, body) = fetch(
        addr,
        b.body(Full::new(Bytes::from(body)))?,
        Duration::from_secs(30),
    )
    .await?;
    Ok((s.as_u16(), h, body))
}

/// Opens one legacy MCP session at `addr``path` (with `key`, when given); its session id.
pub(crate) async fn mcp_session(addr: SocketAddr, path: &str, key: Option<&str>) -> anyhow::Result<String> {
    let mut headers = vec![("mcp-protocol-version", entries::MCP_LEGACY_VERSION.to_owned())];
    if let Some(k) = key {
        headers.push(("authorization", format!("Bearer {k}")));
    }
    let (status, h, body) = post(addr, path, &headers, entries::mcp_initialize()).await?;
    let sid = h
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .with_context(|| {
            format!(
                "initialize at {addr}{path} gave no session ({status}): {}",
                String::from_utf8_lossy(&body)
            )
        })?;
    headers.push(("mcp-session-id", sid.clone()));
    let (status, _, _) = post(addr, path, &headers, entries::mcp_initialized()).await?;
    if !(200..300).contains(&status) {
        bail!("notifications/initialized at {addr}{path} answered {status}");
    }
    Ok(sid)
}

/// Sessions for each key, on the gateway nodes in turn.
pub(crate) async fn mcp_sessions(gw: &Gateway, keys: &[String]) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::with_capacity(keys.len());
    for (i, k) in keys.iter().enumerate() {
        let node = gw.data.get(i % gw.data.len().max(1)).context("no node")?;
        out.push(mcp_session(*node, "/mcp/g", Some(k)).await?);
    }
    Ok(out)
}

/// The configuration version a node serves, from its readiness body.
pub(crate) async fn config_version(addr: SocketAddr) -> Option<u64> {
    let (status, body) = procs::get(addr, READY, None).await.ok()?;
    if status != 200 {
        return None;
    }
    serde_json::from_slice::<Value>(&body)
        .ok()?
        .get("config_version")?
        .as_u64()
}

/// Writes the environment that carries a profile's keys, for an external gateway's operator.
pub(crate) fn write_env(path: &Path, rendered: &Rendered) -> anyhow::Result<()> {
    let text: String = rendered.env.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
}

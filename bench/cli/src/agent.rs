//! `antirion-bench agent`: runs on a gateway or mock machine and does there what the harness does
//! for processes on its own machine: starts them with their files, reads their memory and CPU,
//! finds their children, stops them. It answers only requests carrying the shared token.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::hosts::{self, wire};
use crate::procs::{self, Proc, Spawn};
use crate::{inventory, targets};

struct State {
    token: String,
    work: PathBuf,
    next: AtomicU64,
    procs: Mutex<BTreeMap<u64, Proc>>,
}

type Answer = Result<Response<Full<Bytes>>, Infallible>;

fn reply(status: StatusCode, body: Vec<u8>) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(Bytes::from(body)));
    *r.status_mut() = status;
    r
}

fn ok<T: Serialize>(v: &T) -> anyhow::Result<Response<Full<Bytes>>> {
    Ok(reply(StatusCode::OK, serde_json::to_vec(v)?))
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> anyhow::Result<T> {
    serde_json::from_slice(body).context("bad request body")
}

fn within(work: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        work.join(path)
    }
}

async fn spawn(st: &State, req: wire::SpawnReq) -> anyhow::Result<wire::SpawnResp> {
    for d in &req.clean {
        let d = within(&st.work, d);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).with_context(|| format!("cannot create {}", d.display()))?;
    }
    for (path, text) in &req.files {
        let path = within(&st.work, path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
    }
    let log = within(&st.work, &req.log);
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // The caller names a directory on its own machine, which this one may not have.
    let cwd = req.cwd.as_deref().map(|c| within(&st.work, c));
    if let Some(dir) = &cwd {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let p = procs::spawn(&Spawn {
        name: &req.name,
        program: PathBuf::from(&req.program),
        args: req.args.clone(),
        nice: req.nice,
        cpus: req.cpus.as_deref(),
        env: req.env.as_deref(),
        add_env: &req.add_env,
        cwd: cwd.as_deref(),
        log,
    })?;
    let id = st.next.fetch_add(1, Ordering::Relaxed);
    let resp = wire::SpawnResp {
        id,
        pid: p.pid,
        argv: p.argv.clone(),
    };
    if let Ok(mut m) = st.procs.lock() {
        m.insert(id, p);
    }
    Ok(resp)
}

async fn route(st: &State, path: &str, body: &[u8]) -> anyhow::Result<Response<Full<Bytes>>> {
    match path {
        "/info" => ok(&wire::Info {
            work_dir: st.work.display().to_string(),
        }),
        "/spawn" => ok(&spawn(st, parse(body)?).await?),
        "/stop" => {
            let id: wire::Id = parse(body)?;
            let p = st.procs.lock().ok().and_then(|mut m| m.remove(&id.id));
            if let Some(p) = p {
                p.stop(Duration::from_millis(id.grace_ms)).await;
            }
            ok(&Value::Null)
        }
        "/status" => {
            let id: wire::Id = parse(body)?;
            let exited = st
                .procs
                .lock()
                .ok()
                .and_then(|mut m| m.get_mut(&id.id).map(Proc::exited))
                .unwrap_or_else(|| Some("unknown process".to_owned()));
            ok(&wire::Exited { exited })
        }
        "/read" => {
            let p: wire::Pids = parse(body)?;
            ok(&hosts::local_usage(&p.pids).await)
        }
        "/fds" => {
            let p: wire::Pids = parse(body)?;
            ok(&hosts::local_fds(&p.pids).await)
        }
        "/descendants" => {
            let p: wire::Pids = parse(body)?;
            ok(&targets::with_descendants(&p.pids).await)
        }
        "/match" => {
            let p: wire::Pattern = parse(body)?;
            ok(&targets::matching(&p.pattern).await)
        }
        "/kill" => {
            let p: wire::Pids = parse(body)?;
            procs::kill_all(&p.pids).await;
            ok(&Value::Null)
        }
        "/inventory" => {
            let r: wire::InventoryReq = parse(body)?;
            let mock = r.mock.as_deref().and_then(|m| m.parse().ok());
            let (doc, sysctl, ulimit) = inventory::collect(&r.machine, mock).await?;
            ok(&wire::Inventory { doc, sysctl, ulimit })
        }
        _ => Ok(reply(StatusCode::NOT_FOUND, b"no such path".to_vec())),
    }
}

async fn handle(st: Arc<State>, req: Request<Incoming>) -> Answer {
    let authorized = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| t == st.token);
    if !authorized {
        return Ok(reply(StatusCode::UNAUTHORIZED, b"bad token".to_vec()));
    }
    let path = req.uri().path().to_owned();
    let body = match req.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => return Ok(reply(StatusCode::BAD_REQUEST, e.to_string().into_bytes())),
    };
    Ok(match route(&st, &path, &body).await {
        Ok(r) => r,
        Err(e) => reply(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}").into_bytes()),
    })
}

/// Serves until SIGINT or SIGTERM; the processes it started are stopped on the way out.
pub(crate) async fn serve(listen: SocketAddr, work: PathBuf) -> anyhow::Result<()> {
    let token = std::env::var(hosts::TOKEN_ENV)
        .ok()
        .filter(|t| t.len() >= 16)
        .with_context(|| {
            format!(
                "set {} to a shared token of at least 16 characters",
                hosts::TOKEN_ENV
            )
        })?;
    crate::suite::raise_limits();
    std::fs::create_dir_all(&work)?;
    let work = work.canonicalize().unwrap_or(work);
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("cannot listen on {listen}"))?;
    eprintln!("agent listening on {listen}, work directory {}", work.display());
    let st = Arc::new(State {
        token,
        work,
        next: AtomicU64::new(1),
        procs: Mutex::new(BTreeMap::new()),
    });
    let stop = CancellationToken::new();
    let conns = TaskTracker::new();
    let signal = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let signal = async {
        if let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            tokio::select! {
                () = signal => {}
                _ = term.recv() => {}
            }
        } else {
            signal.await;
        }
    };
    tokio::pin!(signal);
    loop {
        tokio::select! {
            () = &mut signal => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let st = Arc::clone(&st);
                let stop = stop.clone();
                conns.spawn(async move {
                    let svc = service_fn(move |req| handle(Arc::clone(&st), req));
                    let conn = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), svc);
                    tokio::pin!(conn);
                    tokio::select! {
                        _ = conn.as_mut() => {}
                        () = stop.cancelled() => conn.as_mut().graceful_shutdown(),
                    }
                });
            }
        }
    }
    stop.cancel();
    conns.close();
    let _ = tokio::time::timeout(Duration::from_secs(5), conns.wait()).await;
    let left: Vec<Proc> = st
        .procs
        .lock()
        .map(|mut m| std::mem::take(&mut *m).into_values().collect())
        .unwrap_or_default();
    for p in left {
        p.stop(Duration::from_secs(5)).await;
    }
    Ok(())
}

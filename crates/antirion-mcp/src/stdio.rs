//! `stdio` servers as a bounded pool of child processes per node. A process is started lazily,
//! initialized once (it serves the gateway, so with no client capabilities), and then shared by every
//! session: requests are multiplexed over its stdin and stdout by JSON-RPC id. A new process starts
//! only when every running one is busy and the pool is below its size. A process that exits is
//! reaped and replaced after an exponential backoff; one idle for long is stopped. Dropping the pool
//! (a new configuration) stops its processes.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::connect::gateway_client_info;
use crate::http::Env;
use crate::upstream::{self, LATEST_LEGACY, UpError};

const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A process this long without a request is stopped.
const IDLE: Duration = Duration::from_secs(300);
const IDLE_CHECK: Duration = Duration::from_secs(10);
/// The longest line a process may write.
const MAX_LINE: usize = 64 << 20;
const INIT_ID: &str = "antirion-init";

/// One running process.
struct Proc {
    stdin: tokio::sync::Mutex<ChildStdin>,
    waiters: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    in_flight: AtomicUsize,
    alive: AtomicBool,
    last_used: Mutex<Instant>,
    /// The result of the process's `initialize`.
    init: Mutex<Value>,
    stop: CancellationToken,
}

impl Proc {
    fn busy(&self) -> bool {
        self.in_flight.load(Ordering::Relaxed) > 0
    }

    async fn write(&self, msg: &Value) -> Result<(), UpError> {
        let mut line = serde_json::to_vec(msg).map_err(|_| UpError::Rejected)?;
        line.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&line).await.map_err(|_| UpError::Reset)?;
        stdin.flush().await.map_err(|_| UpError::Reset)
    }

    /// Sends a request under `id` and waits for its answer.
    async fn ask(&self, id: &str, mut msg: Value, timeout: Duration) -> Result<Value, UpError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(UpError::Reset);
        }
        if let Some(obj) = msg.as_object_mut() {
            obj.insert("id".into(), Value::from(id));
        }
        let (tx, rx) = oneshot::channel();
        self.waiters.lock().insert(id.to_owned(), tx);
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        let answer = async {
            self.write(&msg).await?;
            match tokio::time::timeout(timeout, rx).await {
                Err(_) => Err(UpError::Timeout),
                Ok(Err(_)) => Err(UpError::Reset),
                Ok(Ok(v)) => Ok(v),
            }
        }
        .await;
        self.waiters.lock().remove(id);
        self.in_flight.fetch_sub(1, Ordering::Relaxed);
        *self.last_used.lock() = Instant::now();
        answer
    }
}

#[derive(Default)]
struct Pool {
    procs: Vec<Arc<Proc>>,
    starting: usize,
    backoff: Option<Duration>,
    not_before: Option<Instant>,
}

/// The pool of one `stdio` server.
pub(crate) struct StdioServer {
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) size: usize,
    pool: Mutex<Pool>,
    next_id: AtomicU64,
    /// How many processes the pool has started, for the status endpoint and tests.
    pub(crate) started: AtomicU64,
    stop: CancellationToken,
}

impl std::fmt::Debug for StdioServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StdioServer")
            .field("command", &self.command)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Drop for StdioServer {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// What to do for one request: use a running process, start one, wait for one being started, or
/// fail while the backoff runs.
enum Plan {
    Use(Arc<Proc>),
    Start,
    Wait,
    Refused,
}

impl StdioServer {
    pub(crate) fn new(
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
        size: usize,
    ) -> StdioServer {
        StdioServer {
            command,
            args,
            env,
            size: size.max(1),
            pool: Mutex::new(Pool::default()),
            next_id: AtomicU64::new(1),
            started: AtomicU64::new(0),
            stop: CancellationToken::new(),
        }
    }

    fn plan(&self) -> Plan {
        let mut pool = self.pool.lock();
        pool.procs.retain(|p| p.alive.load(Ordering::Acquire));
        let idle = pool.procs.iter().find(|p| !p.busy()).cloned();
        if let Some(p) = idle {
            return Plan::Use(p);
        }
        let least = pool
            .procs
            .iter()
            .min_by_key(|p| p.in_flight.load(Ordering::Relaxed))
            .cloned();
        let room = pool.procs.len() + pool.starting < self.size;
        let waiting = pool.not_before.is_some_and(|t| Instant::now() < t);
        match (room && !waiting, least) {
            (true, _) => {
                pool.starting += 1;
                Plan::Start
            }
            (false, Some(p)) => Plan::Use(p),
            (false, None) if pool.starting > 0 => Plan::Wait,
            (false, None) => Plan::Refused,
        }
    }

    /// A process to send to, started if needed.
    async fn acquire(&self, env: &Env, init_timeout: Duration) -> Result<Arc<Proc>, UpError> {
        let deadline = Instant::now() + init_timeout;
        loop {
            match self.plan() {
                Plan::Wait if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Plan::Wait => return Err(UpError::Timeout),
                other => return self.take(env, other, init_timeout).await,
            }
        }
    }

    async fn take(&self, env: &Env, plan: Plan, init_timeout: Duration) -> Result<Arc<Proc>, UpError> {
        match plan {
            Plan::Use(p) => Ok(p),
            Plan::Refused | Plan::Wait => Err(UpError::Connect),
            Plan::Start => {
                let started = self.start(env, init_timeout).await;
                let mut pool = self.pool.lock();
                pool.starting = pool.starting.saturating_sub(1);
                match started {
                    Ok(p) => {
                        pool.procs.push(Arc::clone(&p));
                        pool.backoff = None;
                        pool.not_before = None;
                        Ok(p)
                    }
                    Err(e) => {
                        let next = pool.backoff.map_or(FIRST_BACKOFF, |b| (b * 2).min(MAX_BACKOFF));
                        pool.backoff = Some(next);
                        pool.not_before = Some(Instant::now() + next);
                        Err(e)
                    }
                }
            }
        }
    }

    async fn start(&self, env: &Env, init_timeout: Duration) -> Result<Arc<Proc>, UpError> {
        let mut cmd = Command::new(&self.command);
        cmd.args(&self.args)
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if !self.env.iter().any(|(k, _)| k == "PATH")
            && let Some(path) = std::env::var_os("PATH")
        {
            cmd.env("PATH", path);
        }
        let mut child = cmd.spawn().map_err(|_| UpError::Connect)?;
        self.started.fetch_add(1, Ordering::Relaxed);
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(UpError::Connect);
        };
        let proc = Arc::new(Proc {
            stdin: tokio::sync::Mutex::new(stdin),
            waiters: Mutex::new(HashMap::new()),
            in_flight: AtomicUsize::new(0),
            alive: AtomicBool::new(true),
            last_used: Mutex::new(Instant::now()),
            init: Mutex::new(Value::Null),
            stop: self.stop.child_token(),
        });
        let shutdown = env.shutdown.clone();
        env.tasks.spawn(serve(
            child,
            stdout,
            Arc::downgrade(&proc),
            proc.stop.clone(),
            shutdown,
        ));
        let init = json!({"jsonrpc": "2.0", "method": "initialize", "params": {
            "protocolVersion": LATEST_LEGACY, "capabilities": {}, "clientInfo": gateway_client_info()}});
        let answer = proc.ask(INIT_ID, init, init_timeout).await;
        let result = answer.ok().and_then(|m| m.get("result").cloned()).filter(|r| {
            r.get("protocolVersion")
                .and_then(Value::as_str)
                .is_some_and(upstream::is_legacy)
        });
        let Some(result) = result else {
            proc.stop.cancel();
            return Err(UpError::Rejected);
        };
        *proc.init.lock() = result;
        proc.write(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await?;
        Ok(proc)
    }

    /// The `initialize` result of a process of the pool, starting one if none runs.
    pub(crate) async fn handshake(&self, env: &Env, init_timeout: Duration) -> Result<Value, UpError> {
        let p = self.acquire(env, init_timeout).await?;
        let init = p.init.lock().clone();
        Ok(json!({"jsonrpc": "2.0", "id": INIT_ID, "result": init}))
    }

    /// Sends one request and returns the answer with the request's own id.
    pub(crate) async fn request(
        &self,
        env: &Env,
        msg: &Value,
        init_timeout: Duration,
        timeout: Duration,
    ) -> Result<Value, UpError> {
        let p = self.acquire(env, init_timeout).await?;
        let original = msg.get("id").cloned().unwrap_or(Value::Null);
        let id = format!("antirion-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let mut answer = p.ask(&id, msg.clone(), timeout).await?;
        if let Some(obj) = answer.as_object_mut() {
            obj.insert("id".into(), original);
        }
        Ok(answer)
    }

    /// The processes running now.
    pub(crate) fn running(&self) -> usize {
        self.pool
            .lock()
            .procs
            .iter()
            .filter(|p| p.alive.load(Ordering::Acquire))
            .count()
    }
}

/// Reads a process's answers until it exits, is stopped, idles out or the node shuts down, then
/// stops and reaps it.
async fn serve(
    mut child: Child,
    stdout: ChildStdout,
    proc: std::sync::Weak<Proc>,
    stop: CancellationToken,
    shutdown: CancellationToken,
) {
    let mut lines = BufReader::new(stdout).lines();
    let mut tick = tokio::time::interval(IDLE_CHECK);
    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            () = shutdown.cancelled() => break,
            _ = tick.tick() => {
                let Some(p) = proc.upgrade() else { break };
                if !p.busy() && p.last_used.lock().elapsed() > IDLE {
                    break;
                }
            }
            line = lines.next_line() => match line {
                Ok(Some(line)) if line.len() <= MAX_LINE => {
                    let Some(p) = proc.upgrade() else { break };
                    deliver(&p, &line);
                }
                Ok(Some(_)) | Ok(None) | Err(_) => break,
            },
        }
    }
    if let Some(p) = proc.upgrade() {
        p.alive.store(false, Ordering::Release);
        p.waiters.lock().clear();
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// Hands an answer to the request waiting for its id; other messages are not used.
fn deliver(p: &Proc, line: &str) {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if msg.get("result").is_none() && msg.get("error").is_none() {
        return;
    }
    let Some(id) = msg.get("id").and_then(Value::as_str) else {
        return;
    };
    if let Some(tx) = p.waiters.lock().remove(id) {
        let _ = tx.send(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_starts_only_when_every_process_is_busy() {
        let s = StdioServer::new("/bin/false".into(), Vec::new(), Vec::new(), 2);
        assert!(matches!(s.plan(), Plan::Start));
        assert!(matches!(s.plan(), Plan::Start));
        assert!(
            matches!(s.plan(), Plan::Wait),
            "two are starting: the pool is full"
        );
        assert_eq!(s.running(), 0);
    }
}

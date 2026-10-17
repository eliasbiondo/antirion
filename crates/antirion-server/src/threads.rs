//! The data plane's threads. Each runs a single-threaded runtime and serves every connection handed
//! to it whole: the connection, the tasks its requests spawn and the upstream connections they open
//! all run on that thread, so serving a request wakes no other thread. A new connection goes to the
//! thread serving the fewest. The requests of an HTTP/2 connection are spread over the threads, so
//! one connection's streams are not bound to one CPU.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// The period of the timer every data thread keeps pending.
const NEAR: Duration = Duration::from_millis(250);

/// How long a stopping thread waits for the blocking work its runtime started.
const STOP_GRACE: Duration = Duration::from_secs(1);

struct Lane {
    handle: Handle,
    /// Connections being served on the thread.
    open: Arc<AtomicUsize>,
}

/// The data plane's threads, one per available CPU.
pub struct DataThreads {
    lanes: Vec<Lane>,
    /// The lane the next spread request goes to.
    next: AtomicUsize,
    threads: parking_lot::Mutex<Vec<JoinHandle<()>>>,
    tasks: TaskTracker,
    exit: CancellationToken,
}

impl std::fmt::Debug for DataThreads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataThreads")
            .field("threads", &self.lanes.len())
            .finish_non_exhaustive()
    }
}

/// Counts a connection on its thread until dropped.
struct Occupied(Arc<AtomicUsize>);

impl Drop for Occupied {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl DataThreads {
    /// Starts one thread per CPU available to the process; the tasks they spawn for connections are
    /// tracked by `tasks`.
    pub fn start(tasks: TaskTracker) -> io::Result<DataThreads> {
        let count = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        DataThreads::with_threads(count, tasks)
    }

    fn with_threads(count: usize, tasks: TaskTracker) -> io::Result<DataThreads> {
        let exit = CancellationToken::new();
        let mut lanes = Vec::with_capacity(count);
        let mut threads = Vec::with_capacity(count);
        for i in 0..count.max(1) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let handle = runtime.handle().clone();
            let stop = exit.clone();
            let thread = std::thread::Builder::new()
                .name(format!("antirion-data-{i}"))
                .spawn(move || {
                    runtime.block_on(async {
                        tokio::select! {
                            () = stop.cancelled() => {}
                            () = keep_a_timer_near() => {}
                        }
                    });
                    runtime.shutdown_timeout(STOP_GRACE);
                })?;
            lanes.push(Lane {
                handle,
                open: Arc::new(AtomicUsize::new(0)),
            });
            threads.push(thread);
        }
        Ok(DataThreads {
            lanes,
            next: AtomicUsize::new(0),
            threads: parking_lot::Mutex::new(threads),
            tasks,
            exit,
        })
    }

    /// Serves `stream` with the future `serve` makes from it, on the thread serving the fewest
    /// connections.
    pub(crate) fn serve<F, Fut>(&self, stream: TcpStream, serve: F)
    where
        F: FnOnce(TcpStream) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Some(lane) = self.lanes.iter().min_by_key(|l| l.open.load(Ordering::Relaxed)) else {
            self.tasks.spawn(serve(stream));
            return;
        };
        let std = match stream.into_std() {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = %e, "an accepted connection could not be handed to a data thread");
                return;
            }
        };
        lane.open.fetch_add(1, Ordering::Relaxed);
        let occupied = Occupied(Arc::clone(&lane.open));
        self.tasks.spawn_on(
            async move {
                let _occupied = occupied;
                match TcpStream::from_std(std) {
                    Ok(stream) => serve(stream).await,
                    Err(e) => tracing::debug!(error = %e, "a data thread could not register a connection"),
                }
            },
            &lane.handle,
        );
    }

    /// The threads' runtimes.
    pub(crate) fn handles(&self) -> impl Iterator<Item = &Handle> {
        self.lanes.iter().map(|l| &l.handle)
    }

    /// The thread the next spread request runs on, in turn.
    pub(crate) fn spread(&self) -> Option<&Handle> {
        let i = self.next.fetch_add(1, Ordering::Relaxed);
        self.lanes.get(i % self.lanes.len().max(1)).map(|l| &l.handle)
    }

    /// Stops the threads once the work tracked by the node is done or abandoned; tasks still
    /// running on them are dropped.
    pub async fn stop(&self) {
        self.exit.cancel();
        let threads = std::mem::take(&mut *self.threads.lock());
        let _ = tokio::task::spawn_blocking(move || {
            for t in threads {
                let _ = t.join();
            }
        })
        .await;
    }
}

/// Keeps a timer due within `NEAR`. The timer driver wakes its parked thread (a syscall) whenever
/// a new timer is due before every pending one, which on a thread with no timer pending is every
/// timer a new connection sets; with this one pending, only timers shorter than `NEAR` do.
async fn keep_a_timer_near() {
    loop {
        tokio::time::sleep(NEAR).await;
    }
}

impl Drop for DataThreads {
    fn drop(&mut self) {
        self.exit.cancel();
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connections_are_spread_and_served_on_the_data_threads() {
        let tasks = TaskTracker::new();
        let threads = DataThreads::with_threads(2, tasks.clone()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut clients = Vec::new();
        for _ in 0..4 {
            clients.push(TcpStream::connect(addr).await.unwrap());
            let (stream, _) = listener.accept().await.unwrap();
            threads.serve(stream, |mut s| async move {
                let name = std::thread::current().name().unwrap_or_default().to_owned();
                let mut b = [0u8; 1];
                let _ = s.read_exact(&mut b).await;
                let _ = s.write_all(name.as_bytes()).await;
            });
        }
        let mut names = Vec::new();
        for mut c in clients {
            c.write_all(b"x").await.unwrap();
            let mut name = String::new();
            c.read_to_string(&mut name).await.unwrap();
            names.push(name);
        }
        names.sort();
        assert_eq!(
            names,
            [
                "antirion-data-0",
                "antirion-data-0",
                "antirion-data-1",
                "antirion-data-1"
            ]
        );
        tasks.close();
        tasks.wait().await;
        assert!(threads.lanes.iter().all(|l| l.open.load(Ordering::Relaxed) == 0));
        threads.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spread_requests_take_the_threads_in_turn() {
        let tasks = TaskTracker::new();
        let threads = DataThreads::with_threads(2, tasks.clone()).unwrap();
        let mut names = Vec::new();
        for _ in 0..4 {
            let on = threads.spread().unwrap();
            let name = tasks
                .spawn_on(
                    async { std::thread::current().name().unwrap_or_default().to_owned() },
                    on,
                )
                .await
                .unwrap();
            names.push(name);
        }
        assert_eq!(
            names,
            [
                "antirion-data-0",
                "antirion-data-1",
                "antirion-data-0",
                "antirion-data-1"
            ]
        );
        threads.stop().await;
    }
}

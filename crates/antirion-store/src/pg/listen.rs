//! A dedicated connection that `LISTEN`s on a channel and publishes the highest generation it was
//! told about.

use futures_util::StreamExt;
use tokio::sync::watch;
use tokio_postgres::AsyncMessage;

use super::pool::Pool;
use crate::StoreError;

/// A listening connection. `generations` closes when the connection ends; open a new one then.
#[derive(Debug)]
pub struct Listener {
    pub generations: watch::Receiver<u64>,
    _client: tokio_postgres::Client,
    _driver: tokio::task::JoinSet<()>,
}

pub(crate) async fn open(pool: &Pool, channel: &str) -> Result<Listener, StoreError> {
    let (client, mut connection) = tokio::time::timeout(
        pool.config()
            .get_connect_timeout()
            .copied()
            .unwrap_or(std::time::Duration::from_secs(5)),
        pool.config().connect(pool.tls()),
    )
    .await
    .map_err(|_| StoreError::Timeout)??;
    let (tx, rx) = watch::channel(0u64);
    let mut driver = tokio::task::JoinSet::new();
    driver.spawn(async move {
        let mut messages = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
        while let Some(Ok(message)) = messages.next().await {
            if let AsyncMessage::Notification(n) = message
                && let Ok(generation) = n.payload().parse::<u64>()
            {
                tx.send_if_modified(|current| {
                    let newer = generation > *current;
                    if newer {
                        *current = generation;
                    }
                    newer
                });
            }
        }
    });
    client.batch_execute(&format!("LISTEN {channel}")).await?;
    Ok(Listener {
        generations: rx,
        _client: client,
        _driver: driver,
    })
}

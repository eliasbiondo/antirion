//! Feedback on stored records and the erasure of records, through the node's request log: queued
//! records are written first, so a record this node just produced is found.

use antirion_store::StoreError;
use antirion_store::logs::{FeedbackRow, LogFilter};

use crate::Runtime;

impl Runtime {
    /// Stores one piece of feedback.
    pub async fn add_feedback(&self, row: FeedbackRow) -> Result<(), StoreError> {
        self.store.insert_feedback(row).await
    }

    /// The feedback of one record, oldest first.
    pub async fn feedback_of(&self, log_id: &str) -> Result<Vec<FeedbackRow>, StoreError> {
        self.store.feedback_of(log_id).await
    }

    /// Deletes the records `f` matches, with their feedback; returns how many records went.
    pub async fn erase_logs(&self, f: LogFilter) -> Result<u64, StoreError> {
        self.read_logs().await;
        self.erase_safety_of(&f).await?;
        match self.state.request_logs.clickhouse() {
            Some(c) => {
                self.store.delete_feedback_of(c.ids(&f).await?).await?;
                c.delete(&f).await
            }
            None => self.store.delete_logs(f).await,
        }
    }
}

//! The ledger lines re-costing reads: every line admitted in a range, from the node's window and
//! the store together, or an error when the store cannot be read (a partial read would re-cost
//! lines against an incomplete history).

use std::sync::Arc;

use antirion_core::UtcMicros;
use antirion_limits::book::LedgerFilter;
use antirion_limits::ledger::LedgerLine;
use antirion_store::StoreError;

use crate::Runtime;
use crate::ledger::{decode, merged};

impl Runtime {
    /// The lines admitted in `[from, to)`, oldest admission first.
    pub async fn lines_admitted_between(
        &self,
        from: UtcMicros,
        to: UtcMicros,
    ) -> Result<Vec<Arc<LedgerLine>>, StoreError> {
        let f = LedgerFilter {
            from: Some(from),
            to: Some(to),
            ..LedgerFilter::default()
        };
        let window = self.accounts.book().select(&f);
        let stored = self.store.ledger_between(Some(from.0), Some(to.0)).await?;
        let stored = decode(&stored).into_iter().filter(|l| f.accepts(l)).collect();
        let mut lines = merged(window, stored);
        lines.retain(|l| !self.erased(l));
        Ok(lines)
    }
}

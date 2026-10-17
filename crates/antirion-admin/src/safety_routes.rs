//! The operations of the Safety screen: safety events and their labels, their summary, and the
//! guardrails' statistics.

use crate::Handled;
use crate::call::Call;
use crate::error::AdminError;

/// Whether `segs` is one of these operations.
pub(crate) fn routes(segs: &[&str]) -> bool {
    matches!(
        segs,
        ["guardrails", "events"]
            | ["guardrails", "events", _]
            | ["reports", "guardrails", "summary"]
            | ["runtime", "guardrails"]
    )
}

pub(crate) fn dispatch<'a>(call: &'a Call, segs: &'a [&'a str]) -> Handled<'a> {
    match segs {
        ["guardrails", "events"] => Box::pin(crate::safety::handle(call, None)),
        ["guardrails", "events", segment] => Box::pin(crate::safety::handle(call, Some(segment))),
        ["reports", "guardrails", "summary"] => Box::pin(crate::safety_summary::summary(call)),
        ["runtime", "guardrails"] => Box::pin(crate::guardrail_stats::list(call)),
        _ => crate::routes::done(Err(AdminError::not_found())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_these_operations_are_claimed() {
        assert!(routes(&["guardrails", "events"]) && routes(&["guardrails", "events", "e1:review"]));
        assert!(routes(&["reports", "guardrails", "summary"]) && routes(&["runtime", "guardrails"]));
        assert!(!routes(&["guardrails", "g1"]) && !routes(&["guardrails", "g1:test"]));
    }
}

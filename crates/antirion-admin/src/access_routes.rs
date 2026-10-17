//! The operations of the API keys, Teams and users and API access screens that are not plain
//! resource reads and writes: offboarding a user, key anomaly flags, limit counters and their
//! simulation.

use crate::Handled;
use crate::call::Call;
use crate::error::AdminError;

/// Whether `segs` is one of these operations.
pub(crate) fn routes(segs: &[&str]) -> bool {
    matches!(segs, ["users", action] if action.ends_with(":offboard"))
        || matches!(
            segs,
            ["runtime", "limits:simulate"] | ["runtime", "limits"] | ["anomalies"] | ["anomalies", _]
        )
}

pub(crate) fn dispatch<'a>(call: &'a Call, segs: &'a [&'a str]) -> Handled<'a> {
    match segs {
        ["users", action] if action.ends_with(":offboard") => Box::pin(crate::offboard::offboard(
            call,
            action.trim_end_matches(":offboard"),
        )),
        ["runtime", "limits:simulate"] => Box::pin(crate::limitsim::simulate(call)),
        ["runtime", "limits"] => Box::pin(crate::limit_counters::list(call)),
        ["anomalies"] => Box::pin(crate::anomalies::handle(call, None)),
        ["anomalies", segment] => Box::pin(crate::anomalies::handle(call, Some(segment))),
        _ => crate::routes::done(Err(AdminError::not_found())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_these_operations_are_claimed() {
        assert!(routes(&["users", "alice:offboard"]));
        assert!(routes(&["runtime", "limits:simulate"]));
        assert!(routes(&["anomalies"]) && routes(&["anomalies", "a1:dismiss"]));
        assert!(routes(&["runtime", "limits"]));
        assert!(!routes(&["users", "alice"]) && !routes(&["runtime", "budgets"]));
    }
}

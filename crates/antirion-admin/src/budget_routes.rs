//! The operations of the Budgets screen and of the spend parts of the Overview and Analytics that are
//! not plain budget counter reads: budget events, overage grants and requests, and the generation
//! history the spend charts annotate.

use crate::Handled;
use crate::call::Call;
use crate::error::AdminError;

/// Whether `segs` is one of these operations.
pub(crate) fn routes(segs: &[&str]) -> bool {
    matches!(
        segs,
        ["runtime", "budgets", "events"]
            | ["runtime", "budgets", _, _, _, "grants", _]
            | ["budgets", "overage_requests"]
            | ["budgets", "overage_requests", _]
            | ["config", "generations"]
            | ["config", "generations", _]
            | ["config:rollback"]
    )
}

pub(crate) fn dispatch<'a>(call: &'a Call, segs: &'a [&'a str]) -> Handled<'a> {
    match segs {
        ["runtime", "budgets", "events"] => Box::pin(crate::budget_events::list(call)),
        ["runtime", "budgets", level, owner, name, "grants", id] => {
            Box::pin(crate::overage::revoke(call, level, owner, name, id))
        }
        ["budgets", "overage_requests"] => Box::pin(crate::overage::list(call)),
        ["budgets", "overage_requests", segment] => Box::pin(crate::overage::decide(call, segment)),
        ["config", "generations"] => Box::pin(crate::generations::list(call)),
        ["config", "generations", n] => Box::pin(crate::generations::one(call, n)),
        ["config:rollback"] => Box::pin(crate::generations::rollback(call)),
        _ => crate::routes::done(Err(AdminError::not_found())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_these_operations_are_claimed() {
        assert!(routes(&["runtime", "budgets", "events"]));
        assert!(routes(&["runtime", "budgets", "team", "t", "m", "grants", "g1"]));
        assert!(routes(&["budgets", "overage_requests", "ovr_1:approve"]));
        assert!(routes(&["config", "generations", "3"]) && routes(&["config:rollback"]));
        assert!(!routes(&["runtime", "budgets"]) && !routes(&["runtime", "budgets", "team", "t", "m"]));
        assert!(!routes(&["config"]));
    }
}

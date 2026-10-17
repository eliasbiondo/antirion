//! The lifecycle stages every data-plane request passes, in order.

/// A lifecycle stage. Errors name the stage that failed.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    Accept,
    Authenticate,
    Parse,
    Resolve,
    Admit,
    GuardInput,
    CacheLookup,
    Route,
    Attempt,
    GuardOutput,
    Respond,
    Settle,
}

impl Stage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Stage::Accept => "accept",
            Stage::Authenticate => "authenticate",
            Stage::Parse => "parse",
            Stage::Resolve => "resolve",
            Stage::Admit => "admit",
            Stage::GuardInput => "guard_input",
            Stage::CacheLookup => "cache_lookup",
            Stage::Route => "route",
            Stage::Attempt => "attempt",
            Stage::GuardOutput => "guard_output",
            Stage::Respond => "respond",
            Stage::Settle => "settle",
        }
    }
}

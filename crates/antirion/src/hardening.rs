//! Process hardening, before any secret is read: no core dumps and, on Linux, a process that is not
//! dumpable (so another process of the same user cannot read its memory through `ptrace` or
//! `/proc`), unless `ANTIRION_ALLOW_CORE_DUMPS=1`.

use rustix::process::{Resource, Rlimit, setrlimit};

/// The variable that keeps core dumps on, for debugging.
const ALLOW: &str = "ANTIRION_ALLOW_CORE_DUMPS";

pub(crate) fn harden() {
    if std::env::var(ALLOW).is_ok_and(|v| v == "1") {
        tracing::warn!("{ALLOW}=1: core dumps stay enabled and may contain secrets");
        return;
    }
    let none = Rlimit {
        current: Some(0),
        maximum: Some(0),
    };
    if let Err(e) = setrlimit(Resource::Core, none) {
        tracing::warn!(error = %e, "core dumps could not be disabled");
    }
    #[cfg(target_os = "linux")]
    if let Err(e) = rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable) {
        tracing::warn!(error = %e, "the process could not be made non-dumpable");
    }
}

/// Warns once the configuration is loaded when some secret could not be locked into memory.
pub(crate) fn report_unlocked_secrets() {
    if antirion_core::secret::some_unlocked() {
        tracing::warn!(
            "some secrets could not be locked into memory (the memory-lock limit is too low, or locking is not \
             permitted); they may be written to swap"
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn core_dumps_are_off_after_hardening() {
        super::harden();
        let limit = rustix::process::getrlimit(rustix::process::Resource::Core);
        assert_eq!(limit.current, Some(0));
    }
}

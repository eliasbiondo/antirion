//! The process's registry of built-in extensions: the one a custom build installed at startup, else
//! the extensions every build ships.

use std::sync::OnceLock;

use crate::api::Registry;

static INSTALLED: OnceLock<Registry> = OnceLock::new();

pub(crate) fn install(registry: Registry) -> bool {
    INSTALLED.set(registry).is_ok()
}

/// The registry configurations are compiled against.
pub(crate) fn current() -> &'static Registry {
    INSTALLED.get_or_init(Registry::builtin)
}

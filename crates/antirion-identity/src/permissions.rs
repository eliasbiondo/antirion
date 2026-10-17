//! The closed catalogue of key permissions that unlock request controls.

use std::fmt;

/// A permission a virtual key may hold.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum KeyPermission {
    OverrideRelax,
    ModelsDirectProvider,
    RoutePin,
    RouteDebug,
    RouteDecide,
    RouteFallbacks,
    Byok,
    TagsRequest,
    Debug,
    LimitsReadInherited,
}

impl KeyPermission {
    pub const ALL: [KeyPermission; 10] = [
        KeyPermission::OverrideRelax,
        KeyPermission::ModelsDirectProvider,
        KeyPermission::RoutePin,
        KeyPermission::RouteDebug,
        KeyPermission::RouteDecide,
        KeyPermission::RouteFallbacks,
        KeyPermission::Byok,
        KeyPermission::TagsRequest,
        KeyPermission::Debug,
        KeyPermission::LimitsReadInherited,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            KeyPermission::OverrideRelax => "override:relax",
            KeyPermission::ModelsDirectProvider => "models:direct_provider",
            KeyPermission::RoutePin => "route:pin",
            KeyPermission::RouteDebug => "route:debug",
            KeyPermission::RouteDecide => "route:decide",
            KeyPermission::RouteFallbacks => "route:fallbacks",
            KeyPermission::Byok => "byok",
            KeyPermission::TagsRequest => "tags:request",
            KeyPermission::Debug => "debug",
            KeyPermission::LimitsReadInherited => "limits:read_inherited",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<KeyPermission> {
        KeyPermission::ALL.into_iter().find(|p| p.as_str() == s)
    }

    const fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

impl fmt::Display for KeyPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The permissions granted to one key.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyPermissions(u16);

impl KeyPermissions {
    /// The known permissions among `names`; unknown names are ignored (the schema rejects them).
    pub fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> KeyPermissions {
        KeyPermissions(
            names
                .into_iter()
                .filter_map(KeyPermission::parse)
                .fold(0, |acc, p| acc | p.bit()),
        )
    }

    #[must_use]
    pub fn has(self, p: KeyPermission) -> bool {
        self.0 & p.bit() != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_sets_hold_what_was_granted() {
        for p in KeyPermission::ALL {
            assert_eq!(KeyPermission::parse(p.as_str()), Some(p));
        }
        let set = KeyPermissions::from_names(["debug", "route:pin", "nope"]);
        assert!(set.has(KeyPermission::Debug) && set.has(KeyPermission::RoutePin));
        assert!(!set.has(KeyPermission::OverrideRelax));
        assert!(!KeyPermissions::default().has(KeyPermission::Debug));
    }
}

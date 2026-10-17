//! Counter identities and the holds a reservation takes on them.

use std::fmt;

use antirion_core::ResourceUid;
use sha2::{Digest, Sha256};

use crate::level::Level;
use crate::math::{Gcra, Window};
use crate::period::PeriodBounds;

/// A counter's identity, hashed to 128 bits: kind, level, owner incarnation, declaration name,
/// window and partition values. Owners are incarnations, never names, and partition values are
/// hashed, so no end-user id or address reaches the counter store.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CounterId(pub [u8; 16]);

impl fmt::Debug for CounterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CounterId({})", self.hex())
    }
}

impl CounterId {
    /// Hashes the identity parts. Every part is length-prefixed, so no two identities collide by
    /// concatenation.
    #[must_use]
    pub fn derive(
        kind: &str,
        level: Level,
        owner: ResourceUid,
        owner_path: &str,
        name: &str,
        window: &str,
        partition: &[(String, String)],
    ) -> CounterId {
        let mut h = Sha256::new();
        let mut part = |p: &[u8]| {
            h.update((p.len() as u64).to_le_bytes());
            h.update(p);
        };
        part(kind.as_bytes());
        part(level.as_str().as_bytes());
        part(&owner.0.to_le_bytes());
        part(owner_path.as_bytes());
        part(name.as_bytes());
        part(window.as_bytes());
        for (attr, value) in partition {
            part(attr.as_bytes());
            part(value.as_bytes());
        }
        let digest = h.finalize();
        let mut id = [0u8; 16];
        for (dst, src) in id.iter_mut().zip(digest.iter()) {
            *dst = *src;
        }
        CounterId(id)
    }

    #[must_use]
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Reads [`CounterId::hex`] back.
    #[must_use]
    pub fn from_hex(s: &str) -> Option<CounterId> {
        if s.len() != 32 || !s.is_ascii() {
            return None;
        }
        let mut id = [0u8; 16];
        for (i, b) in id.iter_mut().enumerate() {
            *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        Some(CounterId(id))
    }
}

/// The counting rule of a hold.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Rule {
    Gcra(Gcra),
    Window(Window),
    Concurrency {
        limit: u64,
    },
    /// A budget of `amount` in the period containing the admission time.
    Budget {
        amount: u128,
        period: PeriodBounds,
    },
    /// A rolling budget: at most `limit` (the amount) charged in any window of `per_us`, counted
    /// in `precision` sub-windows; it refuses like a budget.
    Rolling(Window),
}

impl Rule {
    /// Whether the counter is a budget, of a period or rolling.
    #[must_use]
    pub fn is_budget(&self) -> bool {
        matches!(self, Rule::Budget { .. } | Rule::Rolling(_))
    }

    /// The period a budget's alerts, events and spend figures name: its period, or for a rolling
    /// budget the window from its oldest unexpired sub-window, which moves once per sub-window.
    #[must_use]
    pub fn budget_period(&self, now: antirion_core::UtcMicros) -> Option<PeriodBounds> {
        match self {
            Rule::Budget { period, .. } => Some(*period),
            Rule::Rolling(w) => Some(PeriodBounds {
                start: antirion_core::UtcMicros(w.live_from(now.0)),
                end: None,
            }),
            _ => None,
        }
    }

    /// The period start that keys a budget's state; zero for other counters.
    #[must_use]
    pub fn period_start(&self) -> i64 {
        match self {
            Rule::Budget { period, .. } => period.start.0,
            _ => 0,
        }
    }
}

/// How a hold takes part in admission.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum HoldMode {
    /// Must fit; refuses otherwise.
    Enforce,
    /// Applied whether it fits or not, never refuses (shadow limits).
    Shadow,
    /// Not consulted at admission, charged at settlement (soft budgets).
    Track,
    /// Never charged and never refusing: what it has left is kept free in the enforced hold just
    /// before it, which admits only what fits beside that (another priority class's unused share).
    Headroom,
}

/// One counter's part of a reservation.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Hold {
    pub counter: CounterId,
    /// The organization incarnation for tenant-level counters (a store's key grouping), zero for
    /// counters shared across tenants.
    pub group: u128,
    pub rule: Rule,
    /// The reservation `ρ` in the counter's units.
    pub amount: u128,
    pub mode: HoldMode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_differ_by_every_part() {
        let base = || CounterId::derive("limit", Level::Key, ResourceUid(7), "", "rpm", "g:60", &[]);
        assert_eq!(base(), base());
        assert_ne!(
            base(),
            CounterId::derive("budget", Level::Key, ResourceUid(7), "", "rpm", "g:60", &[])
        );
        assert_ne!(
            base(),
            CounterId::derive("limit", Level::Team, ResourceUid(7), "", "rpm", "g:60", &[])
        );
        assert_ne!(
            base(),
            CounterId::derive("limit", Level::Key, ResourceUid(8), "", "rpm", "g:60", &[])
        );
        assert_ne!(
            base(),
            CounterId::derive("limit", Level::Key, ResourceUid(7), "", "rpm", "g:61", &[])
        );
        let p = |v: &str| vec![("end_user".to_owned(), v.to_owned())];
        let a = CounterId::derive("limit", Level::Key, ResourceUid(7), "", "rpm", "g:60", &p("a"));
        assert_ne!(
            a,
            CounterId::derive("limit", Level::Key, ResourceUid(7), "", "rpm", "g:60", &p("b"))
        );
        assert_ne!(
            CounterId::derive("limit", Level::Key, ResourceUid(7), "", "ab", "c", &[]),
            CounterId::derive("limit", Level::Key, ResourceUid(7), "", "a", "bc", &[])
        );
        assert_eq!(a.hex().len(), 32);
        assert_eq!(CounterId::from_hex(&a.hex()), Some(a));
        assert_eq!(CounterId::from_hex("zz"), None);
    }
}

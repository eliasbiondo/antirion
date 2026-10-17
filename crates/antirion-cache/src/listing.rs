//! The entry listing every backend answers: filters, the visibility of the caller, sort orders,
//! keyset positions and totals.

use crate::meta::EntryMeta;

/// What a listing sorts by.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum SortKey {
    #[default]
    LastHit,
    Created,
    Expires,
    Hits,
    Size,
}

impl SortKey {
    #[must_use]
    pub fn parse(s: &str) -> Option<SortKey> {
        Some(match s {
            "last_hit_at" => SortKey::LastHit,
            "created_at" => SortKey::Created,
            "expires_at" => SortKey::Expires,
            "hits" => SortKey::Hits,
            "size" => SortKey::Size,
            _ => return None,
        })
    }
}

/// The value entries never hit sort by under [`SortKey::LastHit`]: below every hit time, in the
/// order they were written. It stays an exact integer as an `f64` score.
pub const NEVER_HIT: i64 = -4_000_000_000_000_000;

/// The last-use sort value of an entry.
#[must_use]
pub fn last_use_value(created_ms: i64, last_hit_ms: Option<i64>) -> i64 {
    last_hit_ms.unwrap_or(NEVER_HIT.saturating_add(created_ms))
}

/// Where a page ends: the sort value and key of its last item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    pub value: i64,
    pub key: String,
}

impl Position {
    /// `<value>:<key>`.
    #[must_use]
    pub fn encode(&self) -> String {
        format!("{}:{}", self.value, self.key)
    }

    #[must_use]
    pub fn decode(s: &str) -> Option<Position> {
        let (value, key) = s.rsplit_once(':')?;
        Some(Position {
            value: value.parse().ok()?,
            key: key.to_owned(),
        })
    }
}

/// A listing request.
#[derive(Clone, Debug, Default)]
pub struct ListQuery {
    /// Dimension values (`model/m`) an entry must all carry.
    pub all_of: Vec<String>,
    /// Dimension values of which an entry must carry one: the caller's scopes (`project/p`); `None`
    /// when the caller sees every entry.
    pub any_of: Option<Vec<String>>,
    /// A prefix of the key.
    pub prefix: Option<String>,
    pub sort: SortKey,
    pub descending: bool,
    pub after: Option<Position>,
    pub limit: usize,
    pub totals: bool,
    pub now_ms: i64,
}

/// One live entry as a listing reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub key: String,
    pub meta: EntryMeta,
    pub deployment: String,
    pub provider: String,
    pub size_bytes: u64,
    pub hits: u64,
    pub created_ms: i64,
    pub expires_ms: i64,
    pub last_hit_ms: Option<i64>,
}

impl Listed {
    /// The item's value under `sort`.
    #[must_use]
    pub fn sort_value(&self, sort: SortKey) -> i64 {
        match sort {
            SortKey::LastHit => last_use_value(self.created_ms, self.last_hit_ms),
            SortKey::Created => self.created_ms,
            SortKey::Expires => self.expires_ms,
            SortKey::Hits => i64::try_from(self.hits).unwrap_or(i64::MAX),
            SortKey::Size => i64::try_from(self.size_bytes).unwrap_or(i64::MAX),
        }
    }
}

/// The sums `include_total` asks for.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub count: u64,
    pub bytes: u64,
    pub hits: u64,
}

/// A page of entries.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub items: Vec<Listed>,
    pub next: Option<Position>,
    pub totals: Option<Totals>,
}

/// Whether `(value, key)` comes after `after` in the listing's order.
#[must_use]
pub fn beyond(value: i64, key: &str, after: Option<&Position>, descending: bool) -> bool {
    let Some(p) = after else {
        return true;
    };
    let ord = (value, key).cmp(&(p.value, p.key.as_str()));
    if descending { ord.is_lt() } else { ord.is_gt() }
}

/// Sorts candidates in the listing's order and cuts the page after `after`.
#[must_use]
pub fn page(mut items: Vec<Listed>, q: &ListQuery) -> (Vec<Listed>, Option<Position>) {
    items.retain(|i| beyond(i.sort_value(q.sort), &i.key, q.after.as_ref(), q.descending));
    items.sort_by(|a, b| {
        let ord = (a.sort_value(q.sort), &a.key).cmp(&(b.sort_value(q.sort), &b.key));
        if q.descending { ord.reverse() } else { ord }
    });
    let more = items.len() > q.limit;
    items.truncate(q.limit);
    let next = more.then(|| items.last()).flatten().map(|i| Position {
        value: i.sort_value(q.sort),
        key: i.key.clone(),
    });
    (items, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(key: &str, created: i64, hit: Option<i64>) -> Listed {
        Listed {
            key: key.into(),
            meta: EntryMeta::default(),
            deployment: String::new(),
            provider: String::new(),
            size_bytes: 1,
            hits: u64::from(hit.is_some()),
            created_ms: created,
            expires_ms: created + 10,
            last_hit_ms: hit,
        }
    }

    #[test]
    fn last_hit_orders_hits_first_then_writes() {
        let q = ListQuery {
            descending: true,
            limit: 2,
            ..ListQuery::default()
        };
        let items = vec![item("a", 1, None), item("b", 2, Some(5)), item("c", 3, None)];
        let (first, next) = page(items.clone(), &q);
        assert_eq!(
            first.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(),
            ["b", "c"]
        );
        let next = next.unwrap();
        assert_eq!(Position::decode(&next.encode()), Some(next.clone()));
        let (rest, end) = page(
            items,
            &ListQuery {
                after: Some(next),
                ..q
            },
        );
        assert_eq!(rest.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(), ["a"]);
        assert!(end.is_none());
    }

    #[test]
    fn never_hit_values_stay_exact_as_scores() {
        let v = last_use_value(1_800_000_000_000, None);
        assert_eq!(v as f64 as i64, v);
        assert!(v < 0);
    }
}

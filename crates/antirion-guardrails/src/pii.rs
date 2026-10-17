//! PII entities: candidate patterns, the checks that confirm them (Luhn, mod-97, number ranges) and
//! the rule that a match never touches an ASCII letter or digit.

use std::sync::LazyLock;

use regex::Regex;

use crate::pattern::Span;

/// A PII entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entity {
    Email,
    Phone,
    CreditCard,
    UsSsn,
    Iban,
    IpAddress,
    CaSin,
}

pub(crate) const ALL: [Entity; 7] = [
    Entity::Email,
    Entity::Phone,
    Entity::CreditCard,
    Entity::UsSsn,
    Entity::Iban,
    Entity::IpAddress,
    Entity::CaSin,
];

fn re(p: &str) -> Option<Regex> {
    Regex::new(p).ok()
}

static EMAIL: LazyLock<Option<Regex>> = LazyLock::new(|| {
    re(
        r"[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\.[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]+)*@(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?\.)+[A-Za-z]{2,24}",
    )
});
static PHONE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    re(
        r"\+[0-9](?:[ .\-]?[0-9]){7,14}|\([2-9][0-9]{2}\) [2-9][0-9]{2}-[0-9]{4}|[2-9][0-9]{2}-[2-9][0-9]{2}-[0-9]{4}|[2-9][0-9]{2}\.[2-9][0-9]{2}\.[0-9]{4}",
    )
});
static CARD: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"[0-9](?:[ \-]?[0-9]){12,18}"));
static SSN: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"[0-9]{3}-[0-9]{2}-[0-9]{4}"));
static IBAN: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"[A-Z]{2}[0-9]{2}(?:(?: [A-Z0-9]{4}){2,7}(?: [A-Z0-9]{1,3})?|[A-Z0-9]{11,30})"));
static IPV4: LazyLock<Option<Regex>> = LazyLock::new(|| {
    re(r"(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])(?:\.(?:25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])){3}")
});
static IPV6: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"[0-9A-Fa-f:.]*:[0-9A-Fa-f:.]*"));
static SIN: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"[0-9]{3}-[0-9]{3}-[0-9]{3}|[0-9]{3} [0-9]{3} [0-9]{3}|[0-9]{9}"));

impl Entity {
    pub(crate) fn parse(s: &str) -> Option<Entity> {
        ALL.into_iter().find(|e| e.name() == s)
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Entity::Email => "email",
            Entity::Phone => "phone",
            Entity::CreditCard => "credit_card",
            Entity::UsSsn => "us_ssn",
            Entity::Iban => "iban",
            Entity::IpAddress => "ip_address",
            Entity::CaSin => "ca_sin",
        }
    }

    /// The longest match in code points.
    #[must_use]
    pub const fn max_len(self) -> usize {
        match self {
            Entity::Email => 254,
            Entity::Phone => 24,
            Entity::CreditCard => 23,
            Entity::UsSsn | Entity::CaSin => 11,
            Entity::Iban => 42,
            Entity::IpAddress => 45,
        }
    }

    fn pattern(self) -> Option<&'static Regex> {
        match self {
            Entity::Email => EMAIL.as_ref(),
            Entity::Phone => PHONE.as_ref(),
            Entity::CreditCard => CARD.as_ref(),
            Entity::UsSsn => SSN.as_ref(),
            Entity::Iban => IBAN.as_ref(),
            Entity::IpAddress => IPV4.as_ref(),
            Entity::CaSin => SIN.as_ref(),
        }
    }

    fn valid(self, m: &str) -> bool {
        let digits: Vec<u32> = m.chars().filter_map(|c| c.to_digit(10)).collect();
        match self {
            Entity::Email => {
                let local = m.split('@').next().unwrap_or_default();
                local.len() <= 64 && m.len() <= 254
            }
            Entity::Phone | Entity::IpAddress => true,
            Entity::CreditCard => (13..=19).contains(&digits.len()) && luhn(&digits),
            Entity::CaSin => digits.len() == 9 && luhn(&digits),
            Entity::UsSsn => {
                let num =
                    |r: std::ops::Range<usize>| digits.get(r).map(|d| d.iter().fold(0, |a, x| a * 10 + x));
                let (Some(area), Some(group), Some(serial)) = (num(0..3), num(3..5), num(5..9)) else {
                    return false;
                };
                area != 0 && area != 666 && area < 900 && group != 0 && serial != 0
            }
            Entity::Iban => iban_ok(m),
        }
    }

    /// Every confirmed occurrence in `text`.
    pub(crate) fn spans(self, text: &str) -> Vec<Span> {
        let mut out = Vec::new();
        if let Some(re) = self.pattern() {
            for m in re.find_iter(text) {
                if bounded(text, m.start(), m.end()) && self.valid(m.as_str()) {
                    out.push(self.span(m.start(), m.end()));
                }
            }
        }
        if self == Entity::IpAddress
            && let Some(re) = IPV6.as_ref()
        {
            for m in re.find_iter(text) {
                let s = m.as_str().trim_end_matches('.');
                if s.matches(':').count() >= 2
                    && s.parse::<std::net::Ipv6Addr>().is_ok()
                    && bounded(text, m.start(), m.start() + s.len())
                {
                    out.push(self.span(m.start(), m.start() + s.len()));
                }
            }
        }
        out
    }

    fn span(self, start: usize, end: usize) -> Span {
        Span {
            start,
            end,
            label: self.name().to_ascii_uppercase(),
        }
    }
}

/// The match is not preceded or followed by an ASCII letter or digit.
fn bounded(text: &str, start: usize, end: usize) -> bool {
    let before = text.get(..start).and_then(|s| s.chars().next_back());
    let after = text.get(end..).and_then(|s| s.chars().next());
    !before.is_some_and(|c| c.is_ascii_alphanumeric()) && !after.is_some_and(|c| c.is_ascii_alphanumeric())
}

pub(crate) fn luhn(digits: &[u32]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            if i % 2 == 1 {
                let x = d * 2;
                if x > 9 { x - 9 } else { x }
            } else {
                *d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

pub(crate) fn iban_ok(m: &str) -> bool {
    let compact: String = m.chars().filter(|c| !c.is_whitespace()).collect();
    if !(15..=34).contains(&compact.len()) {
        return false;
    }
    let (head, tail) = compact.split_at(4);
    let mut rem: u64 = 0;
    for c in tail.chars().chain(head.chars()) {
        let v = match c.to_digit(36) {
            Some(v) => v,
            None => return false,
        };
        rem = if v >= 10 {
            (rem * 100 + u64::from(v)) % 97
        } else {
            (rem * 10 + u64::from(v)) % 97
        };
    }
    rem == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detects(e: Entity, s: &str) -> bool {
        let text = format!("x {s} y");
        e.spans(&text).iter().any(|sp| &text[sp.start..sp.end] == s)
    }

    #[test]
    fn must_detect_and_must_not_detect() {
        let table: [(Entity, &[&str], &[&str]); 7] = [
            (Entity::Email, &["a.b+c@ex-ample.co.uk"], &["a@b", "@example.com"]),
            (
                Entity::Phone,
                &["+44 20 7946 0958", "(415) 555-0132"],
                &["2024-01-15"],
            ),
            (
                Entity::CreditCard,
                &["4111 1111 1111 1111"],
                &["4111 1111 1111 1112"],
            ),
            (Entity::UsSsn, &["123-45-6789"], &["000-12-3456", "123456789"]),
            (
                Entity::Iban,
                &["GB82 WEST 1234 5698 7654 32"],
                &["GB00 WEST 1234 5698 7654 32"],
            ),
            (Entity::IpAddress, &["192.168.0.1", "2001:db8::1"], &["256.1.1.1"]),
            (Entity::CaSin, &["046 454 286"], &["046 454 287"]),
        ];
        for (e, yes, no) in table {
            for s in yes {
                assert!(detects(e, s), "{e:?} must detect {s}");
            }
            for s in no {
                let text = format!("x {s} y");
                assert!(e.spans(&text).is_empty(), "{e:?} must not detect {s}");
            }
        }
    }

    #[test]
    fn a_match_never_touches_a_letter_or_digit() {
        assert!(Entity::UsSsn.spans("id x123-45-6789").is_empty());
        assert!(Entity::UsSsn.spans("id 123-45-67890").is_empty());
        assert!(Entity::CreditCard.spans("n x4111 1111 1111 1111").is_empty());
    }
}

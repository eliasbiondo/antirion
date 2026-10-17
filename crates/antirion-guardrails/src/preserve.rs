//! Format-preserving masks: the parts of a PII value a `pii` guardrail may keep beside its
//! replacement (`pii.preserve`), each too little to identify the value on its own.

use std::borrow::Cow;
use std::net::Ipv6Addr;

use crate::pii::Entity;

/// A part of a value that a redaction keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preserve {
    /// The `@domain` of an email address.
    EmailDomain,
    /// The /24 network of an IPv4 address, the /48 of an IPv6 address.
    IpSubnet,
    /// The last four characters of a card or account number.
    Last4,
    /// The calling code of an international phone number.
    Country,
}

impl Preserve {
    pub(crate) fn parse(s: &str) -> Option<Preserve> {
        Some(match s {
            "email_domain" => Preserve::EmailDomain,
            "ip_subnet" => Preserve::IpSubnet,
            "last4" => Preserve::Last4,
            "country" => Preserve::Country,
            _ => return None,
        })
    }
}

/// The settings of a `pii` guardrail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiiRules {
    pub entities: Vec<Entity>,
    pub preserve: Vec<Preserve>,
    /// Input values become numbered tokens that the output stage turns back into the values.
    pub reversible: bool,
}

/// A matched value cut into what is kept before, the part that is replaced, and what is kept
/// after.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Cut<'a> {
    pub(crate) before: Cow<'a, str>,
    pub(crate) replaced: &'a str,
    pub(crate) after: &'a str,
}

impl<'a> Cut<'a> {
    fn whole(matched: &'a str) -> Cut<'a> {
        Cut {
            before: Cow::Borrowed(""),
            replaced: matched,
            after: "",
        }
    }
}

/// Two-digit country calling codes (ITU-T E.164); `1` and `7` are the one-digit codes and every
/// other code has three digits.
const TWO_DIGIT_CODES: [u8; 44] = [
    20, 27, 30, 31, 32, 33, 34, 36, 39, 40, 41, 43, 44, 45, 46, 47, 48, 49, 51, 52, 53, 54, 55, 56, 57, 58,
    60, 61, 62, 63, 64, 65, 66, 81, 82, 84, 86, 90, 91, 92, 93, 94, 95, 98,
];

/// The length in digits of the calling code at the start of `digits`.
fn calling_code_len(digits: &[u8]) -> usize {
    match digits {
        [b'1' | b'7', ..] => 1,
        [a, b, ..] if TWO_DIGIT_CODES.contains(&((a - b'0') * 10 + (b - b'0'))) => 2,
        _ => 3,
    }
}

fn phone(matched: &str) -> Option<Cut<'_>> {
    let rest = matched.strip_prefix('+')?;
    let digits: Vec<u8> = rest.bytes().filter(u8::is_ascii_digit).take(3).collect();
    let n = calling_code_len(&digits);
    let mut seen = 0;
    let mut end = 1;
    for (i, b) in rest.bytes().enumerate() {
        if seen == n {
            if matches!(b, b' ' | b'.' | b'-') {
                end = i + 2;
            }
            break;
        }
        if b.is_ascii_digit() {
            seen += 1;
        }
        end = i + 2;
    }
    let (before, replaced) = matched.split_at_checked(end)?;
    Some(Cut {
        before: Cow::Borrowed(before),
        replaced,
        after: "",
    })
}

fn last4(matched: &str) -> Option<Cut<'_>> {
    let (at, _) = matched
        .char_indices()
        .rev()
        .filter(|(_, c)| c.is_ascii_alphanumeric())
        .nth(3)?;
    let (replaced, after) = matched.split_at_checked(at)?;
    Some(Cut {
        before: Cow::Borrowed(""),
        replaced,
        after,
    })
}

fn subnet(matched: &str) -> Option<Cut<'_>> {
    if matched.contains(':') {
        let ip: Ipv6Addr = matched.parse().ok()?;
        let [a, b, c, ..] = ip.segments();
        return Some(Cut {
            before: Cow::Owned(format!("{a:x}:{b:x}:{c:x}:")),
            replaced: matched,
            after: "",
        });
    }
    let (at, _) = matched.match_indices('.').nth(2)?;
    let (before, replaced) = matched.split_at_checked(at + 1)?;
    Some(Cut {
        before: Cow::Borrowed(before),
        replaced,
        after: "",
    })
}

/// How a value matched as `label` (an uppercase entity name) is cut under `keep`; the whole value
/// is replaced when nothing applies to its entity.
pub(crate) fn cut<'a>(label: &str, matched: &'a str, keep: &[Preserve]) -> Cut<'a> {
    let found = match label {
        "EMAIL" if keep.contains(&Preserve::EmailDomain) => matched.rfind('@').map(|at| Cut {
            before: Cow::Borrowed(""),
            replaced: matched.get(..at).unwrap_or(matched),
            after: matched.get(at..).unwrap_or_default(),
        }),
        "IP_ADDRESS" if keep.contains(&Preserve::IpSubnet) => subnet(matched),
        "CREDIT_CARD" | "IBAN" if keep.contains(&Preserve::Last4) => last4(matched),
        "PHONE" if keep.contains(&Preserve::Country) => phone(matched),
        _ => None,
    };
    found.unwrap_or_else(|| Cut::whole(matched))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(label: &str, v: &str, keep: Preserve) -> (String, String, String) {
        let c = cut(label, v, &[keep]);
        (c.before.into_owned(), c.replaced.to_owned(), c.after.to_owned())
    }

    #[test]
    fn each_preserved_part_is_kept() {
        assert_eq!(
            shown("EMAIL", "ana@acme.example", Preserve::EmailDomain),
            (String::new(), "ana".into(), "@acme.example".into())
        );
        assert_eq!(
            shown("IP_ADDRESS", "192.168.0.17", Preserve::IpSubnet),
            ("192.168.0.".into(), "17".into(), String::new())
        );
        assert_eq!(
            shown("IP_ADDRESS", "2001:db8::1", Preserve::IpSubnet).0,
            "2001:db8:0:"
        );
        assert_eq!(
            shown("CREDIT_CARD", "4111 1111 1111 1111", Preserve::Last4),
            (String::new(), "4111 1111 1111 ".into(), "1111".into())
        );
        assert_eq!(
            shown("PHONE", "+44 20 7946 0958", Preserve::Country),
            ("+44 ".into(), "20 7946 0958".into(), String::new())
        );
        assert_eq!(shown("PHONE", "+1-415-555-0132", Preserve::Country).0, "+1-");
        assert_eq!(shown("PHONE", "+351912345678", Preserve::Country).0, "+351");
    }

    #[test]
    fn nothing_is_kept_where_the_part_does_not_apply() {
        let c = cut("PHONE", "(415) 555-0132", &[Preserve::Country]);
        assert_eq!(c, Cut::whole("(415) 555-0132"));
        let c = cut("EMAIL", "ana@acme.example", &[Preserve::Last4]);
        assert_eq!(c, Cut::whole("ana@acme.example"));
    }

    #[test]
    fn calling_codes_have_one_two_or_three_digits() {
        assert_eq!(calling_code_len(b"1415"), 1);
        assert_eq!(calling_code_len(b"447"), 2);
        assert_eq!(calling_code_len(b"351"), 3);
    }
}

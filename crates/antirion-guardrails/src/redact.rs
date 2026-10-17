//! Redaction: spans of one guardrail merged, then replaced right to left by a label, a mask or a
//! keyed hash.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::def::Replacement;
use crate::pattern::Span;
use crate::preserve::{Preserve, cut};

/// Sorts spans and merges overlapping or adjacent ones (the first span's label wins).
#[must_use]
pub fn merge(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans {
        match out.last_mut() {
            Some(last) if s.start <= last.end => last.end = last.end.max(s.end),
            _ => out.push(s),
        }
    }
    out
}

/// What replaces `matched`.
#[must_use]
pub fn replacement(matched: &str, label: &str, how: Replacement, hash_key: Option<&[u8]>) -> String {
    match how {
        Replacement::Label => format!("[{}]", label.to_uppercase()),
        Replacement::Mask => "*".repeat(matched.chars().count()),
        Replacement::Hash => {
            let digest = hash_key
                .and_then(|k| <Hmac<Sha256> as KeyInit>::new_from_slice(k).ok())
                .map(|mut mac| {
                    mac.update(matched.as_bytes());
                    mac.finalize().into_bytes()
                });
            let hex: String = digest
                .map(|d| d.iter().take(4).map(|b| format!("{b:02x}")).collect())
                .unwrap_or_default();
            format!("[{}:{hex}]", label.to_uppercase())
        }
    }
}

/// What replaces `matched`, keeping the parts `keep` lists in place.
#[must_use]
pub fn masked(
    matched: &str,
    label: &str,
    how: Replacement,
    hash_key: Option<&[u8]>,
    keep: &[Preserve],
) -> String {
    if keep.is_empty() {
        return replacement(matched, label, how, hash_key);
    }
    let c = cut(label, matched, keep);
    format!(
        "{}{}{}",
        c.before,
        replacement(c.replaced, label, how, hash_key),
        c.after
    )
}

/// How the spans of one guardrail are replaced.
#[derive(Clone, Copy, Debug)]
pub struct Style<'a> {
    pub how: Replacement,
    pub hash_key: Option<&'a [u8]>,
    pub keep: &'a [Preserve],
}

/// `text[from..to]` with every span inside the range replaced. Spans must not straddle the range.
#[must_use]
pub fn render(text: &str, spans: &[Span], from: usize, to: usize, style: Style<'_>) -> String {
    let mut out = String::new();
    let mut at = from;
    for s in spans.iter().filter(|s| s.start >= from && s.end <= to) {
        out.push_str(text.get(at..s.start).unwrap_or_default());
        out.push_str(&masked(
            text.get(s.start..s.end).unwrap_or_default(),
            &s.label,
            style.how,
            style.hash_key,
            style.keep,
        ));
        at = s.end;
    }
    out.push_str(text.get(at..to).unwrap_or_default());
    out
}

/// The whole text with its spans replaced.
#[must_use]
pub fn apply(text: &str, spans: &[Span], style: Style<'_>) -> String {
    render(text, spans, 0, text.len(), style)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: usize, end: usize, label: &str) -> Span {
        Span {
            start,
            end,
            label: label.into(),
        }
    }

    #[test]
    fn spans_merge_and_replace() {
        let text = "mail ana@example.com now";
        let spans = merge(vec![span(5, 20, "email"), span(10, 12, "x")]);
        assert_eq!(spans.len(), 1);
        let style = |how, hash_key| Style {
            how,
            hash_key,
            keep: &[],
        };
        assert_eq!(
            apply(text, &spans, style(Replacement::Label, None)),
            "mail [EMAIL] now"
        );
        assert_eq!(
            apply(text, &spans, style(Replacement::Mask, None)),
            format!("mail {} now", "*".repeat(15))
        );
        let hashed = apply(text, &spans, style(Replacement::Hash, Some(b"k")));
        assert!(hashed.starts_with("mail [EMAIL:") && hashed.len() == "mail [EMAIL:12345678] now".len());
    }

    #[test]
    fn preserved_parts_stay_beside_the_replacement() {
        let keep = [Preserve::EmailDomain];
        let style = |how| Style {
            how,
            hash_key: None,
            keep: &keep,
        };
        let text = "write to ana@acme.example";
        let spans = vec![span(9, 25, "EMAIL")];
        assert_eq!(
            apply(text, &spans, style(Replacement::Label)),
            "write to [EMAIL]@acme.example"
        );
        assert_eq!(
            apply(text, &spans, style(Replacement::Mask)),
            "write to ***@acme.example"
        );
    }
}

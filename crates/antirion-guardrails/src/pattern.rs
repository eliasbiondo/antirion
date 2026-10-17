//! The `regex` and `keywords` checks: patterns compiled at load under the size limits, their longest
//! match computed from the syntax tree, and the spans they find.

use antirion_config::{ConfigError, DetailCode, DocPath};
use regex::{Regex, RegexBuilder};
use regex_syntax::hir::{Hir, HirKind};
use serde_json::Value;

/// The compiled size limit of one pattern.
const PATTERN_SIZE_LIMIT: usize = 1 << 20;
/// The compiled size limit of a keyword list (up to 100 000 words).
const KEYWORD_SIZE_LIMIT: usize = 256 << 20;

/// A matched span: byte offsets into the segment text and the replacement label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    /// The entity or label the replacement names (`EMAIL`, `REDACTED`).
    pub label: String,
}

/// The checksum a deny match must pass to count (`regex.validate`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Validate {
    None,
    /// The match's digits pass the Luhn check.
    Luhn,
    /// The match is an IBAN whose ISO 13616 mod-97 remainder is 1.
    IbanMod97,
}

impl Validate {
    fn holds(self, matched: &str) -> bool {
        match self {
            Validate::None => true,
            Validate::Luhn => {
                let digits: Vec<u32> = matched.chars().filter_map(|c| c.to_digit(10)).collect();
                digits.len() >= 2 && crate::pii::luhn(&digits)
            }
            Validate::IbanMod97 => crate::pii::iban_ok(matched),
        }
    }
}

/// A `regex` guardrail.
#[derive(Debug)]
pub struct RegexCheck {
    deny: Vec<Regex>,
    allow: Vec<Regex>,
    pub(crate) label: String,
    validate: Validate,
    /// The longest deny match in code points, when every deny pattern is bounded and there is no
    /// allow list.
    max: Option<usize>,
}

/// The longest match of a pattern in code points; `None` when unbounded.
fn max_chars(h: &Hir) -> Option<usize> {
    match h.kind() {
        HirKind::Empty | HirKind::Look(_) => Some(0),
        HirKind::Literal(l) => Some(
            std::str::from_utf8(&l.0)
                .map(|s| s.chars().count())
                .unwrap_or(l.0.len()),
        ),
        HirKind::Class(_) => Some(1),
        HirKind::Repetition(r) => {
            let times = usize::try_from(r.max?).ok()?;
            max_chars(&r.sub)?.checked_mul(times)
        }
        HirKind::Capture(c) => max_chars(&c.sub),
        HirKind::Concat(v) => v.iter().try_fold(0usize, |acc, h| acc.checked_add(max_chars(h)?)),
        HirKind::Alternation(v) => v.iter().try_fold(0usize, |acc, h| Some(acc.max(max_chars(h)?))),
    }
}

/// Compiles one pattern, reporting why it cannot be used.
fn compile_pattern(p: &str) -> Result<(Regex, Option<usize>), String> {
    let re = RegexBuilder::new(p)
        .size_limit(PATTERN_SIZE_LIMIT)
        .build()
        .map_err(|e| match e {
            regex::Error::CompiledTooBig(_) => "the pattern compiles to more than 1 MiB".to_owned(),
            regex::Error::Syntax(s) => format!("the pattern is not valid: {}", first_line(&s)),
            _ => "the pattern is not valid".to_owned(),
        })?;
    let max = regex_syntax::ParserBuilder::new()
        .build()
        .parse(p)
        .ok()
        .and_then(|h| max_chars(&h));
    Ok((re, max))
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| l.starts_with("error:"))
        .unwrap_or("syntax error")
        .to_owned()
}

fn strings(v: Option<&Value>) -> impl Iterator<Item = (usize, &str)> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, s)| s.as_str().map(|s| (i, s)))
}

impl RegexCheck {
    pub(crate) fn compile(block: Option<&Value>, at: &DocPath, errors: &mut Vec<ConfigError>) -> RegexCheck {
        let mut bounded = Some(0usize);
        let mut list = |member: &str, track: bool, bounded: &mut Option<usize>| -> Vec<Regex> {
            let mut out = Vec::new();
            for (i, p) in strings(block.and_then(|b| b.get(member))) {
                match compile_pattern(p) {
                    Ok((re, max)) => {
                        if track {
                            *bounded = match (*bounded, max) {
                                (Some(a), Some(b)) => Some(a.max(b)),
                                _ => None,
                            };
                        }
                        out.push(re);
                    }
                    Err(why) => errors.push(ConfigError::new(
                        &at.key("regex").key(member).index(i),
                        DetailCode::InvalidFormat,
                        why,
                    )),
                }
            }
            out
        };
        let deny = list("deny", true, &mut bounded);
        let allow = list("allow", false, &mut bounded);
        let label = block
            .and_then(|b| b.get("label"))
            .and_then(Value::as_str)
            .unwrap_or("REDACTED")
            .to_owned();
        let max = if allow.is_empty() { bounded } else { None };
        let validate = match block.and_then(|b| b.get("validate")).and_then(Value::as_str) {
            Some("luhn") => Validate::Luhn,
            Some("iban_mod97") => Validate::IbanMod97,
            _ => Validate::None,
        };
        RegexCheck {
            deny,
            allow,
            label,
            validate,
            max,
        }
    }

    pub(crate) fn max_len(&self) -> Option<usize> {
        self.max
    }

    pub(crate) fn has_allow(&self) -> bool {
        !self.allow.is_empty()
    }

    /// Every match of every deny pattern.
    pub(crate) fn deny_spans(&self, text: &str) -> Vec<Span> {
        self.deny
            .iter()
            .flat_map(|re| re.find_iter(text))
            .filter(|m| m.end() > m.start() && self.validate.holds(m.as_str()))
            .map(|m| Span {
                start: m.start(),
                end: m.end(),
                label: self.label.clone(),
            })
            .collect()
    }

    /// Whether some allow pattern matches `text`.
    pub(crate) fn allows(&self, text: &str) -> bool {
        self.allow.iter().any(|re| re.is_match(text))
    }

    /// The index of every deny pattern longer than `limit` code points (or unbounded).
    pub(crate) fn longer_than(&self, block: Option<&Value>, limit: usize) -> Vec<usize> {
        strings(block.and_then(|b| b.get("deny")))
            .filter(|(_, p)| {
                compile_pattern(p)
                    .ok()
                    .is_none_or(|(_, m)| m.is_none_or(|m| m > limit))
            })
            .map(|(i, _)| i)
            .collect()
    }
}

/// A `keywords` guardrail.
#[derive(Debug)]
pub struct KeywordCheck {
    re: Option<Regex>,
    words: bool,
    max: usize,
    pub(crate) label: String,
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl KeywordCheck {
    pub(crate) fn compile(
        block: Option<&Value>,
        at: &DocPath,
        errors: &mut Vec<ConfigError>,
    ) -> KeywordCheck {
        let mut words: Vec<&str> = strings(block.and_then(|b| b.get("words")))
            .map(|(_, w)| w)
            .collect();
        words.sort_by_key(|w| std::cmp::Reverse(w.chars().count()));
        let max = words.iter().map(|w| w.chars().count()).max().unwrap_or(0);
        let alternation = words
            .iter()
            .map(|w| regex::escape(w))
            .collect::<Vec<_>>()
            .join("|");
        let re = if words.is_empty() {
            None
        } else {
            match RegexBuilder::new(&alternation)
                .case_insensitive(true)
                .size_limit(KEYWORD_SIZE_LIMIT)
                .build()
            {
                Ok(re) => Some(re),
                Err(_) => {
                    errors.push(ConfigError::new(
                        &at.key("keywords").key("words"),
                        DetailCode::OutOfRange,
                        "the keyword list compiles to more than the size limit",
                    ));
                    None
                }
            }
        };
        let substring = block.and_then(|b| b.get("match")).and_then(Value::as_str) == Some("substring");
        KeywordCheck {
            re,
            words: !substring,
            max,
            label: "REDACTED".to_owned(),
        }
    }

    pub(crate) fn max_len(&self) -> usize {
        self.max
    }

    /// Every occurrence, as a whole word unless `match: substring`.
    pub(crate) fn spans(&self, text: &str) -> Vec<Span> {
        let Some(re) = &self.re else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut at = 0;
        while at <= text.len() {
            let Some(m) = re.find_at(text, at) else { break };
            let before = text.get(..m.start()).and_then(|s| s.chars().next_back());
            let after = text.get(m.end()..).and_then(|s| s.chars().next());
            let bounded = !self.words || (!before.is_some_and(word_char) && !after.is_some_and(word_char));
            if bounded && m.end() > m.start() {
                out.push(Span {
                    start: m.start(),
                    end: m.end(),
                    label: self.label.clone(),
                });
                at = m.end();
            } else {
                at = next_boundary(text, m.start());
            }
        }
        out
    }
}

/// The byte offset of the character after the one at `i`.
pub(crate) fn next_boundary(text: &str, i: usize) -> usize {
    text.get(i..)
        .and_then(|s| s.chars().next())
        .map_or(text.len() + 1, |c| i + c.len_utf8())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn regex(deny: &[&str]) -> (RegexCheck, Vec<ConfigError>) {
        let mut errors = Vec::new();
        let c = RegexCheck::compile(
            Some(&json!({"deny": deny})),
            &DocPath::root().key("g"),
            &mut errors,
        );
        (c, errors)
    }

    #[test]
    fn unsupported_syntax_is_rejected_with_the_index() {
        for bad in ["[]", "(a)\\1", "(?=a)b", "(?<=a)b", "(?!a)b", "(unclosed"] {
            let (_, errors) = regex(&["ok", bad]);
            assert_eq!(errors.len(), 1, "{bad}");
            assert_eq!(errors[0].path, "g.regex.deny[1]");
        }
        let (_, errors) = regex(&["[\\p{L}\\p{N}]{1000}"]);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn a_checksum_filters_deny_matches() {
        let mut errors = Vec::new();
        let at = DocPath::root().key("g");
        let luhn = RegexCheck::compile(
            Some(&json!({"deny": ["\\b(?:\\d[ -]?){13,19}\\b"], "validate": "luhn"})),
            &at,
            &mut errors,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(luhn.deny_spans("card 4111 1111 1111 1111 ok").len(), 1);
        assert!(luhn.deny_spans("card 4111 1111 1111 1112 ok").is_empty());
        let iban = RegexCheck::compile(
            Some(&json!({"deny": ["[A-Z]{2}[0-9]{2}[A-Z0-9 ]{11,30}[A-Z0-9]"], "validate": "iban_mod97"})),
            &at,
            &mut errors,
        );
        assert_eq!(iban.deny_spans("GB82 WEST 1234 5698 7654 32").len(), 1);
        assert!(iban.deny_spans("GB00 WEST 1234 5698 7654 32").is_empty());
    }

    #[test]
    fn maximum_match_length_comes_from_the_syntax_tree() {
        assert_eq!(regex(&["falcon.{0,10}"]).0.max_len(), Some(16));
        assert_eq!(regex(&["a.*b"]).0.max_len(), None);
        assert_eq!(regex(&["(?i)project\\s+falcon"]).0.max_len(), None);
        assert_eq!(regex(&["é{2}|abc"]).0.max_len(), Some(3));
    }

    #[test]
    fn keywords_respect_word_boundaries_and_fold_case() {
        let mut errors = Vec::new();
        let k = KeywordCheck::compile(
            Some(&json!({"words": ["forbidden"]})),
            &DocPath::root(),
            &mut errors,
        );
        for (text, n) in [
            ("FORBIDDEN!", 1),
            ("forbiddenness", 0),
            ("unforbidden", 0),
            ("forbidden_zone", 0),
            ("forbidden2", 0),
            ("(forbidden)", 1),
            ("forbid", 0),
        ] {
            assert_eq!(k.spans(text).len(), n, "{text}");
        }
        let k = KeywordCheck::compile(Some(&json!({"words": ["école"]})), &DocPath::root(), &mut errors);
        assert_eq!(k.spans("à l'ÉCOLE demain").len(), 1);
        let k = KeywordCheck::compile(
            Some(&json!({"words": ["forbidden"], "match": "substring"})),
            &DocPath::root(),
            &mut errors,
        );
        assert_eq!(k.spans("unforbiddenness").len(), 1);
        assert!(errors.is_empty());
    }
}

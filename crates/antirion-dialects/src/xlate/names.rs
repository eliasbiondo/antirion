//! Tool names and tool-call ids made valid for a target, and mapped back in its responses.

use std::collections::HashMap;

use regex::Regex;

use super::target::{NameRule, Upstream};
use super::util::sha_prefix;

/// A character class of a simple name pattern, as a list of ranges.
#[derive(Clone, Debug, Default)]
struct Class(Vec<(char, char)>);

impl Class {
    fn parse(body: &str) -> Class {
        let chars: Vec<char> = body.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while let Some(&c) = chars.get(i) {
            let c = if c == '\\' {
                i += 1;
                chars.get(i).copied().unwrap_or('\\')
            } else {
                c
            };
            if chars.get(i + 1) == Some(&'-')
                && let Some(&end) = chars.get(i + 2)
            {
                out.push((c, end));
                i += 3;
            } else {
                out.push((c, c));
                i += 1;
            }
        }
        Class(out)
    }

    fn contains(&self, c: char) -> bool {
        self.0.iter().any(|(a, b)| (*a..=*b).contains(&c))
    }
}

/// The classes of a pattern of the form `^[A]{m,n}$` or `^[A][B]{m,n}$`: the first character's and
/// the others'.
fn classes(pattern: &str) -> Option<(Class, Class)> {
    let body = pattern.strip_prefix('^')?.strip_suffix('$')?;
    let mut sets = Vec::new();
    let mut rest = body;
    while let Some(r) = rest.strip_prefix('[') {
        let end = r.find(']')?;
        sets.push(Class::parse(r.get(..end)?));
        rest = r.get(end + 1..)?;
        if let Some(q) = rest.strip_prefix('{') {
            rest = q.get(q.find('}')? + 1..)?;
        } else if let Some(q) = rest.strip_prefix(['+', '*']) {
            rest = q;
        }
    }
    if !rest.is_empty() {
        return None;
    }
    match sets.len() {
        1 => {
            let c = sets.pop()?;
            Some((c.clone(), c))
        }
        2 => {
            let rest = sets.pop()?;
            let first = sets.pop()?;
            Some((first, rest))
        }
        _ => None,
    }
}

/// The tool names of one request for one target: original → sent, and back.
#[derive(Clone, Debug, Default)]
pub struct Names {
    up: HashMap<String, String>,
    back: HashMap<String, String>,
}

struct Checker {
    regex: Option<Regex>,
    classes: (Class, Class),
    max_len: usize,
}

impl Checker {
    fn new(rule: &NameRule) -> Checker {
        Checker {
            regex: Regex::new(&rule.pattern).ok(),
            classes: classes(&rule.pattern)
                .unwrap_or_else(|| (Class::parse("a-zA-Z0-9_-"), Class::parse("a-zA-Z0-9_-"))),
            max_len: rule.max_len.max(10),
        }
    }

    fn valid(&self, name: &str) -> bool {
        name.chars().count() <= self.max_len && self.regex.as_ref().is_none_or(|r| r.is_match(name))
    }

    fn replace(&self, name: &str) -> String {
        name.chars()
            .enumerate()
            .map(|(i, c)| {
                let class = if i == 0 { &self.classes.0 } else { &self.classes.1 };
                if class.contains(c) { c } else { '_' }
            })
            .collect()
    }

    fn hashed(&self, original: &str, replaced: &str) -> String {
        let keep: String = replaced.chars().take(self.max_len - 9).collect();
        format!("{keep}_{}", sha_prefix(original, 8))
    }
}

impl Names {
    /// Assigns a valid name to each distinct name, in order (tool definitions first).
    #[must_use]
    pub fn build<'a>(rule: &NameRule, names: impl IntoIterator<Item = &'a str>) -> Names {
        let check = Checker::new(rule);
        let originals: Vec<&str> = {
            let mut seen: Vec<&str> = Vec::new();
            for n in names {
                if !seen.contains(&n) {
                    seen.push(n);
                }
            }
            seen
        };
        let mut out = Names::default();
        let mut taken: Vec<String> = originals
            .iter()
            .filter(|n| check.valid(n))
            .map(|n| (*n).to_owned())
            .collect();
        for original in &originals {
            let sent = if check.valid(original) {
                (*original).to_owned()
            } else {
                let replaced = check.replace(original);
                let fits = check.valid(&replaced) && !taken.contains(&replaced);
                let sent = if fits {
                    replaced
                } else {
                    check.hashed(original, &replaced)
                };
                taken.push(sent.clone());
                sent
            };
            out.back.insert(sent.clone(), (*original).to_owned());
            out.up.insert((*original).to_owned(), sent);
        }
        out
    }

    /// The name sent upstream for a client name.
    #[must_use]
    pub fn up(&self, name: &str) -> String {
        self.up.get(name).cloned().unwrap_or_else(|| name.to_owned())
    }

    /// The client's name for a name the upstream returned (unknown names are returned as sent).
    #[must_use]
    pub fn client(&self, sent: &str) -> String {
        self.back.get(sent).cloned().unwrap_or_else(|| sent.to_owned())
    }
}

/// Tool-call ids rewritten for a target, and back.
#[derive(Clone, Debug, Default)]
pub struct Ids {
    up: HashMap<String, String>,
    back: HashMap<String, String>,
}

fn id_valid(dialect: Upstream, id: &str) -> bool {
    let chars_ok = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    match dialect {
        Upstream::Anthropic => chars_ok,
        Upstream::Converse => chars_ok && id.len() <= 64,
        _ => true,
    }
}

impl Ids {
    /// The id sent for a client id, recorded for the way back.
    pub fn up(&mut self, dialect: Upstream, id: &str) -> String {
        if id_valid(dialect, id) {
            return id.to_owned();
        }
        let sent = format!("toolu_{}", sha_prefix(id, 24));
        self.back.insert(sent.clone(), id.to_owned());
        self.up.insert(id.to_owned(), sent.clone());
        sent
    }

    /// The client's id for an id the upstream returned.
    #[must_use]
    pub fn client(&self, sent: &str) -> String {
        self.back.get(sent).cloned().unwrap_or_else(|| sent.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xlate::target::default_names;

    #[test]
    fn rewrites_and_collisions() {
        let rule = default_names(Upstream::Chat);
        let n = Names::build(&rule, ["a.b", "a_b"]);
        assert_eq!(n.up("a.b"), format!("a_b_{}", sha_prefix("a.b", 8)));
        assert_eq!(n.up("a_b"), "a_b");
        let long = format!("my.tool.{}", "x".repeat(70));
        let n = Names::build(&rule, [long.as_str()]);
        let sent = n.up(&long);
        assert_eq!(sent.len(), 64);
        assert!(sent.starts_with("my_tool_x"));
        assert_eq!(n.client(&sent), long);
        let g = Names::build(&default_names(Upstream::Gemini), ["my.tool", "1abc"]);
        assert_eq!(g.up("my.tool"), "my.tool");
        assert_eq!(g.up("1abc"), "_abc");
    }

    #[test]
    fn ids() {
        let mut ids = Ids::default();
        let sent = ids.up(Upstream::Anthropic, "call:abc/1");
        assert_eq!(sent, format!("toolu_{}", sha_prefix("call:abc/1", 24)));
        assert_eq!(ids.client(&sent), "call:abc/1");
        assert_eq!(ids.up(Upstream::Anthropic, "call_Ab-9"), "call_Ab-9");
    }
}

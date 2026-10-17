//! Glob patterns over names and URIs: `*` matches any run of characters, everything else is literal.

/// Whether `name` matches `pattern`.
#[must_use]
pub(crate) fn matches(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return name.is_empty();
    };
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let tail: Vec<&str> = parts.collect();
    let Some((last, middle)) = tail.split_last() else {
        return rest.is_empty();
    };
    for piece in middle {
        match rest.find(piece) {
            Some(at) => rest = rest.get(at + piece.len()..).unwrap_or_default(),
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// Whether `name` matches any of `patterns`.
#[must_use]
pub(crate) fn any(patterns: &[String], name: &str) -> bool {
    patterns.iter().any(|p| matches(p, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_and_wildcards() {
        assert!(matches("add", "add"));
        assert!(!matches("add", "adds"));
        assert!(matches("*", ""));
        assert!(matches("get_*", "get_issue"));
        assert!(!matches("get_*", "list_issue"));
        assert!(matches("a__s*", "a__sub"));
        assert!(matches("file:///docs/*", "file:///docs/a/b.md"));
        assert!(!matches("file:///docs/*", "file:///secret.txt"));
        assert!(matches("*x*y", "aaxbby"));
        assert!(!matches("*x*y", "aaybbx"));
        assert!(matches("a*a", "aa"));
        assert!(!matches("a*a", "a"));
    }
}

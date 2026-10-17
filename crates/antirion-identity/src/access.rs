//! Allow- and deny-lists of the tenancy hierarchy: model, provider and deployment patterns.

/// Whether `name` matches a pattern: an exact name, `*` alone, or one `*` at the start or the end.
/// Matching is case-sensitive and on the name exactly as sent.
#[must_use]
pub fn pattern_matches(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return !prefix.contains('*') && name.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return !suffix.contains('*') && name.ends_with(suffix);
    }
    pattern == name
}

/// One level's allow and deny lists for one kind of name. `allow: None` imposes no restriction;
/// `Some(vec![])` denies everything.
#[derive(Clone, Debug, Default)]
pub struct ListPair {
    pub allow: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
}

/// Whether `name` passes every level: it matches each defined allow-list (intersection) and no deny
/// list (union).
#[must_use]
pub fn allowed(name: &str, levels: &[&ListPair]) -> bool {
    levels.iter().all(|l| {
        let allow_ok = l
            .allow
            .as_ref()
            .is_none_or(|a| a.iter().any(|p| pattern_matches(p, name)));
        let denied = l
            .deny
            .as_ref()
            .is_some_and(|d| d.iter().any(|p| pattern_matches(p, name)));
        allow_ok && !denied
    })
}

/// Whether a deployment in `region` passes every level's region lists. A deployment whose region is
/// unknown passes only when no level has an allow-list; deny lists cannot name it.
#[must_use]
pub fn region_allowed(region: Option<&str>, levels: &[&ListPair]) -> bool {
    match region {
        Some(r) => allowed(r, levels),
        None => levels.iter().all(|l| l.allow.is_none()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_without_a_known_value_pass_only_unrestricted_levels() {
        let eu = pair(Some(&["eu-west-1"]), None);
        let no_us = pair(None, Some(&["us-east-1"]));
        assert!(region_allowed(Some("eu-west-1"), &[&eu, &no_us]));
        assert!(!region_allowed(Some("us-east-1"), &[&no_us]));
        assert!(!region_allowed(None, &[&eu]));
        assert!(region_allowed(None, &[&no_us]));
    }

    fn pair(allow: Option<&[&str]>, deny: Option<&[&str]>) -> ListPair {
        let own = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect();
        ListPair {
            allow: allow.map(own),
            deny: deny.map(own),
        }
    }

    #[test]
    fn patterns() {
        assert!(pattern_matches("*", "anything"));
        assert!(pattern_matches("claude-*", "claude-opus"));
        assert!(pattern_matches("*-mini", "gpt-5-mini"));
        assert!(pattern_matches("gpt-5", "gpt-5"));
        assert!(!pattern_matches("gpt-5", "GPT-5"));
        assert!(!pattern_matches("claude-*", "gpt"));
    }

    #[test]
    fn intersection_of_allows_union_of_denies() {
        let org = pair(Some(&["m*"]), None);
        let key = pair(Some(&["mrt"]), Some(&["mx"]));
        assert!(allowed("mrt", &[&org, &key]));
        assert!(!allowed("mrt_other", &[&org, &key]));
        assert!(!allowed("anything", &[&pair(Some(&[]), None)]));
        assert!(allowed("anything", &[&pair(None, None)]));
        assert!(!allowed("mx", &[&pair(None, Some(&["mx"]))]));
    }
}

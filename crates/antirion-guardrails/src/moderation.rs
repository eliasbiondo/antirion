//! Moderation checks: segments cut into overlapping windows, sent in batches, and the answer read
//! into a finding mapped to the closed category list.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::def::Category;

/// Consecutive windows of at most `max` code points overlapping by `overlap`.
#[must_use]
pub fn windows(text: &str, max: usize, overlap: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return vec![text.to_owned()];
    }
    let step = max.saturating_sub(overlap).max(1);
    let mut out = Vec::new();
    let mut start = 0;
    loop {
        let end = (start + max).min(chars.len());
        out.push(chars.get(start..end).unwrap_or_default().iter().collect());
        if end == chars.len() {
            break;
        }
        start += step;
    }
    out
}

/// The `results` of a moderation answer, which must hold one result per input.
pub fn results(status: u16, body: &[u8], inputs: usize) -> Result<Vec<Value>, String> {
    if status != 200 {
        return Err(format!("the moderation call answered with status {status}"));
    }
    let v: Value =
        serde_json::from_slice(body).map_err(|_| "the moderation answer is not JSON".to_owned())?;
    match v.get("results").and_then(Value::as_array) {
        Some(r) if r.len() == inputs => Ok(r.clone()),
        Some(r) => Err(format!("{} results for {inputs} inputs", r.len())),
        None => Err("the moderation answer has no results".to_owned()),
    }
}

/// The top-level group of a provider category (`self-harm/intent` → `self_harm`).
#[must_use]
pub fn map_category(provider: &str) -> Category {
    let group = provider.split('/').next().unwrap_or_default().replace('-', "_");
    match group.as_str() {
        "hate" => Category::Hate,
        "harassment" => Category::Harassment,
        "violence" => Category::Violence,
        "sexual" => Category::Sexual,
        "self_harm" => Category::SelfHarm,
        "illicit" => Category::Illicit,
        _ => Category::Custom,
    }
}

/// The finding of a set of results: the reported category and the provider category (logged).
#[must_use]
pub fn finding(results: &[Value], thresholds: &BTreeMap<String, f64>) -> Option<(Category, String)> {
    let mut triggering: Vec<String> = Vec::new();
    for r in results {
        if thresholds.is_empty() {
            if r.get("flagged").and_then(Value::as_bool) != Some(true) {
                continue;
            }
            let named: Vec<String> = r
                .get("categories")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter(|(_, v)| v.as_bool() == Some(true))
                .map(|(k, _)| k.clone())
                .collect();
            if named.is_empty() {
                triggering.push(String::new());
            }
            triggering.extend(named);
        } else {
            let scores = r.get("category_scores");
            for (c, limit) in thresholds {
                if scores
                    .and_then(|s| s.get(c))
                    .and_then(Value::as_f64)
                    .is_some_and(|s| s >= *limit)
                {
                    triggering.push(c.clone());
                }
            }
        }
    }
    let first = triggering.into_iter().min()?;
    Some((map_category(&first), first))
}

/// The highest score any result gives the provider category `name`.
#[must_use]
pub fn top_score(results: &[Value], name: &str) -> Option<f64> {
    results
        .iter()
        .filter_map(|r| {
            r.pointer("/category_scores")
                .and_then(|s| s.get(name))
                .and_then(Value::as_f64)
        })
        .reduce(f64::max)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn windows_overlap() {
        let text: String = "a".repeat(20000);
        let w = windows(&text, 8000, 200);
        assert_eq!(w.iter().map(String::len).collect::<Vec<_>>(), [8000, 8000, 4400]);
        assert_eq!(windows("short", 8000, 200), ["short"]);
    }

    #[test]
    fn categories_map_to_their_group_and_the_smallest_wins() {
        let r = [json!({"flagged": true, "categories": {"violence": true, "hate": true}})];
        assert_eq!(finding(&r, &BTreeMap::new()).unwrap().0, Category::Hate);
        assert_eq!(map_category("self-harm/intent"), Category::SelfHarm);
        assert_eq!(map_category("totally-new-category"), Category::Custom);
        let t = BTreeMap::from([("violence".to_owned(), 0.5)]);
        let r = [json!({"flagged": false, "category_scores": {"violence": 0.6}})];
        assert_eq!(finding(&r, &t).unwrap().0, Category::Violence);
        let r = [
            json!({"flagged": true, "categories": {"violence": true}, "category_scores": {"violence": 0.4}}),
        ];
        assert!(finding(&r, &t).is_none());
    }
}

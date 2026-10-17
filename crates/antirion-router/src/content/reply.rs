//! Reading what classifiers and scorers answer: cosine scores of embeddings, an LLM's class name,
//! a judge's grade; and the fixed prompts those Models are asked with.

use serde_json::Value;

/// The cosine similarity of two vectors; `None` when their sizes differ or one is zero.
#[must_use]
pub fn cosine(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let norm = na.sqrt() * nb.sqrt();
    (norm > 0.0 && norm.is_finite()).then(|| dot / norm)
}

/// The class of an embedded query: each class scores its best cosine similarity to one of its
/// examples, and the highest score at or above the class's threshold wins (the earlier class on a
/// tie). Returns the winning class, if any, with its score (else the highest score seen); `None`
/// overall when a vector's size does not match the query's.
#[must_use]
pub fn best_class<V: AsRef<[f32]>>(
    query: &[f32],
    examples: &[Vec<V>],
    thresholds: &[f64],
) -> Option<(Option<usize>, f64)> {
    let mut winner: Option<(usize, f64)> = None;
    let mut top = f64::NEG_INFINITY;
    for (class, vectors) in examples.iter().enumerate() {
        let mut best = f64::NEG_INFINITY;
        for v in vectors {
            best = best.max(cosine(query, v.as_ref())?);
        }
        top = top.max(best);
        let threshold = thresholds.get(class).copied().unwrap_or(1.0);
        if best >= threshold && winner.is_none_or(|(_, s)| best > s) {
            winner = Some((class, best));
        }
    }
    Some(match winner {
        Some((c, s)) => (Some(c), s),
        None => (None, top),
    })
}

fn class_index(name: &str, classes: &[String]) -> Option<usize> {
    let name = name.trim().trim_matches(|c| matches!(c, '"' | '\'' | '`' | '.'));
    if name.is_empty() {
        return None;
    }
    classes
        .iter()
        .position(|c| c == name)
        .or_else(|| classes.iter().position(|c| c.eq_ignore_ascii_case(name)))
}

/// The class an LLM named: a JSON object `{"class": "<name>"}` or a bare class name. Anything
/// else names no class.
#[must_use]
pub fn llm_class(reply: &str, classes: &[String]) -> Option<usize> {
    let text = reply.trim();
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(o)) => o
            .get("class")
            .and_then(Value::as_str)
            .and_then(|c| class_index(c, classes)),
        Ok(Value::String(s)) => class_index(&s, classes),
        Ok(_) => None,
        Err(_) => class_index(text, classes),
    }
}

/// A judge's grade from 1 to 5 as a win rate `(g − 1) / 4`: a bare integer or `{"grade": g}`.
#[must_use]
pub fn grade(reply: &str) -> Option<f64> {
    let text = reply.trim();
    let g = match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(o)) => o.get("grade").and_then(Value::as_f64),
        Ok(Value::Number(n)) => n.as_f64(),
        Ok(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => text.parse::<f64>().ok(),
    }?;
    (g.fract() == 0.0 && (1.0..=5.0).contains(&g)).then(|| (g - 1.0) / 4.0)
}

/// The system prompt of an LLM classifier.
#[must_use]
pub fn prompt_for_classes(classes: &[String], descriptions: &[String]) -> String {
    let mut out = String::from(
        "You route requests. Read the user's message and choose the one class that fits it best.\n\
         Answer with a JSON object {\"class\": \"<name>\"} and nothing else. \
         If no class fits, answer {\"class\": \"\"}.\nClasses:\n",
    );
    for (i, name) in classes.iter().enumerate() {
        let d = descriptions.get(i).map(String::as_str).unwrap_or_default();
        if d.is_empty() {
            out.push_str(&format!("- {name}\n"));
        } else {
            out.push_str(&format!("- {name}: {d}\n"));
        }
    }
    out
}

/// The system prompt of an LLM scorer.
#[must_use]
pub fn prompt_for_grade() -> &'static str {
    "You estimate how much a request needs the strongest available model. Read the user's message \
     and grade it from 1 (a small model answers it as well) to 5 (only the strongest model can \
     answer it well). Answer with the grade as a single digit and nothing else."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_best_class_at_or_above_its_threshold_wins() {
        let ex = vec![vec![vec![1.0, 0.0]], vec![vec![0.0, 1.0], vec![0.7, 0.7]]];
        assert_eq!(best_class(&[1.0, 0.0], &ex, &[0.9, 0.9]).unwrap().0, Some(0));
        assert_eq!(best_class(&[0.0, 1.0], &ex, &[0.9, 0.9]).unwrap().0, Some(1));
        assert_eq!(best_class(&[1.0, 0.5], &ex, &[0.85, 0.9]).unwrap().0, Some(1));
        assert_eq!(best_class(&[1.0, 0.5], &ex, &[0.95, 0.95]).unwrap().0, None);
        assert!(best_class(&[1.0, 0.0, 0.0], &ex, &[0.5, 0.5]).is_none());
        let tie = vec![vec![vec![1.0, 0.0]], vec![vec![2.0, 0.0]]];
        assert_eq!(best_class(&[1.0, 0.0], &tie, &[0.5, 0.5]).unwrap().0, Some(0));
    }

    #[test]
    fn llm_answers_name_a_class_or_none() {
        let classes = vec!["code_generation".to_owned(), "chat".to_owned()];
        assert_eq!(llm_class(r#"{"class":"code_generation"}"#, &classes), Some(0));
        assert_eq!(llm_class(" chat\n", &classes), Some(1));
        assert_eq!(llm_class(r#"{"class":""}"#, &classes), None);
        assert_eq!(llm_class(r#"{"route":[]}"#, &classes), None);
        assert_eq!(llm_class("something else", &classes), None);
    }

    #[test]
    fn grades_are_integers_from_one_to_five() {
        assert_eq!(grade("5"), Some(1.0));
        assert_eq!(grade(r#"{"grade": 3}"#), Some(0.5));
        assert_eq!(grade("1"), Some(0.0));
        assert_eq!(grade("6"), None);
        assert_eq!(grade("2.5"), None);
        assert_eq!(grade("high"), None);
    }
}

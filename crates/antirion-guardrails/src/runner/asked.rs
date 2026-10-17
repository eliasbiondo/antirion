//! The checks that ask one of the gateway's own Models: `llm_rewrite` (new texts for the segments)
//! and `topic` (embeddings compared with the topic descriptions).

use super::conclude::Concluded;
use super::{GRACE, Net, StageCx};
use crate::def::{Category, Def, SegKind};
use crate::segments::Segment;

/// Asks the rewrite Model for new texts of the segments in scope.
pub(super) async fn rewrite<N: Net>(
    def: &Def,
    cx: &StageCx<'_, N>,
    r: &crate::rewrite::Rewrite,
    segs: &[(usize, &Segment)],
) -> Concluded {
    let texts: Vec<&str> = segs.iter().map(|(_, s)| s.text.as_str()).collect();
    if texts.is_empty() {
        return Concluded::pass();
    }
    let user = crate::rewrite::Rewrite::message(&texts);
    let asked = cx.net.chat(&r.model, &r.prompt, &user, r.max_tokens, def.timeout);
    let answer = match tokio::time::timeout(def.timeout + GRACE, asked).await {
        Err(_) => return Concluded::failure(def, "timeout".to_owned()),
        Ok(Err(e)) => return Concluded::failure(def, e),
        Ok(Ok(a)) => a,
    };
    let rewritten = match crate::rewrite::rewritten(&answer, texts.len()) {
        Ok(r) => r,
        Err(e) => return Concluded::failure(def, e),
    };
    let edits: Vec<(usize, String)> = rewritten
        .into_iter()
        .filter_map(|(n, text)| {
            let (i, seg) = segs.get(n)?;
            (seg.text != text).then_some((*i, text))
        })
        .collect();
    if edits.is_empty() {
        return Concluded::pass();
    }
    Concluded {
        edits,
        ..Concluded::finding(def, Category::Custom, "rewritten".to_owned())
    }
}

/// The text of the last user message among the segments in scope.
fn last_user_text(segs: &[(usize, &Segment)]) -> Option<String> {
    let last = segs
        .iter()
        .filter(|(_, s)| s.kind == SegKind::User)
        .filter_map(|(_, s)| s.message)
        .max()?;
    let parts: Vec<&str> = segs
        .iter()
        .filter(|(_, s)| s.kind == SegKind::User && s.message == Some(last))
        .map(|(_, s)| s.text.as_str())
        .collect();
    Some(parts.join("\n"))
}

/// The last user message's segments, whole, named by the topic that decided: what the safety
/// events of topic findings are counted by.
fn last_user_spans(segs: &[(usize, &Segment)], label: &str) -> Vec<super::Located> {
    let Some(last) = segs
        .iter()
        .filter(|(_, s)| s.kind == SegKind::User)
        .filter_map(|(_, s)| s.message)
        .max()
    else {
        return Vec::new();
    };
    segs.iter()
        .filter(|(_, s)| s.kind == SegKind::User && s.message == Some(last))
        .map(|(i, s)| super::Located {
            segment: *i,
            start: 0,
            end: s.text.chars().count(),
            entity: label.to_owned(),
        })
        .collect()
}

/// Embeds the last user message (and, the first time, the topic descriptions) and compares them.
pub(super) async fn topic<N: Net>(
    def: &Def,
    cx: &StageCx<'_, N>,
    t: &crate::topic::Topic,
    segs: &[(usize, &Segment)],
) -> Concluded {
    let Some(query) = last_user_text(segs) else {
        return Concluded::pass();
    };
    let known = t.known();
    let mut inputs = vec![query];
    if known.is_none() {
        inputs.extend(t.descriptions());
    }
    let n = inputs.len();
    let asked = cx.net.embed(&t.embedding_model, inputs, def.timeout);
    let mut vectors = match tokio::time::timeout(def.timeout + GRACE, asked).await {
        Err(_) => return Concluded::failure(def, "timeout".to_owned()),
        Ok(Err(e)) => return Concluded::failure(def, e),
        Ok(Ok(v)) if v.len() == n => v,
        Ok(Ok(_)) => return Concluded::failure(def, "the embedding answer has the wrong length".to_owned()),
    };
    let query = vectors.remove(0);
    let described = match known {
        Some(k) => k,
        None => t.remember(vectors),
    };
    match t.judge(&query, &described) {
        crate::topic::Judged::Pass => Concluded::pass(),
        crate::topic::Judged::OffTopic(detail, score) => {
            let label = t.label(&detail);
            let mut c =
                Concluded::finding_unredactable(def, Category::OffTopic, detail).scored(Some(score), 1.0);
            c.spans = last_user_spans(segs, &label);
            c
        }
    }
}

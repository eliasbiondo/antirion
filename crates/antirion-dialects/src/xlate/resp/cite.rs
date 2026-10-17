//! Citations and web searches in responses: what each upstream reports (Anthropic citations and
//! `server_tool_use`, Chat and Responses `url_citation` annotations and `web_search_call` items,
//! Gemini grounding metadata) and what each client receives.

use serde_json::{Map, Value, json};

/// What a citation points at.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A web page.
    Url { url: String, title: Option<String> },
    /// An Anthropic document citation (`char_location`, `page_location`, `content_block_location`
    /// and the like), as Anthropic sent it.
    Document(Value),
}

/// A citation of a span of the candidate's text: character offsets into its text blocks joined.
#[derive(Clone, Debug, PartialEq)]
pub struct Cite {
    pub start: usize,
    pub end: usize,
    pub source: Source,
}

/// A web search the upstream ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Search {
    pub id: String,
    pub query: Option<String>,
}

/// A citation as Anthropic reports it on a text block: web results become URL sources, anything
/// else is a document citation.
pub(crate) fn from_anthropic(c: &Value, start: usize, end: usize) -> Cite {
    let source = match c.get("type").and_then(Value::as_str) {
        Some("web_search_result_location") => Source::Url {
            url: c
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            title: c.get("title").and_then(Value::as_str).map(str::to_owned),
        },
        _ => Source::Document(c.clone()),
    };
    Cite { start, end, source }
}

/// A `url_citation` annotation (Chat's nested form or Responses' flat form) at `offset` characters
/// into the candidate's text.
pub(crate) fn from_annotation(a: &Value, offset: usize) -> Option<Cite> {
    if a.get("type").and_then(Value::as_str) != Some("url_citation") {
        return None;
    }
    let f = a.get("url_citation").unwrap_or(a);
    let index = |k: &str| {
        f.get(k)
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
    };
    Some(Cite {
        start: offset + index("start_index")?,
        end: offset + index("end_index")?,
        source: Source::Url {
            url: f
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            title: f.get("title").and_then(Value::as_str).map(str::to_owned),
        },
    })
}

/// Gemini grounding metadata: one URL citation per supported segment and grounding chunk, with the
/// segment's byte offsets turned into character offsets of `text`; and the queries run.
pub(crate) fn from_grounding(g: &Value, text: &str) -> (Vec<Cite>, Vec<Search>) {
    let chunks: Vec<(String, Option<String>)> = g
        .get("groundingChunks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("web"))
        .map(|w| {
            (
                w.get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                w.get("title").and_then(Value::as_str).map(str::to_owned),
            )
        })
        .collect();
    let chars_at = |byte: usize| text.get(..byte.min(text.len())).map_or(0, |s| s.chars().count());
    let mut cites = Vec::new();
    for s in g
        .get("groundingSupports")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let seg = s.get("segment");
        let byte = |k: &str| {
            seg.and_then(|x| x.get(k))
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
        };
        let (start, end) = (
            chars_at(byte("startIndex").unwrap_or(0)),
            chars_at(byte("endIndex").unwrap_or(0)),
        );
        for i in s
            .get("groundingChunkIndices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
        {
            if let Some((url, title)) = usize::try_from(i).ok().and_then(|i| chunks.get(i)) {
                cites.push(Cite {
                    start,
                    end,
                    source: Source::Url {
                        url: url.clone(),
                        title: title.clone(),
                    },
                });
            }
        }
    }
    let searches = g
        .get("webSearchQueries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .enumerate()
        .map(|(i, q)| Search {
            id: format!("ws_{i}"),
            query: Some(q.to_owned()),
        })
        .collect();
    (cites, searches)
}

/// A Chat annotation.
pub(crate) fn chat_annotation(c: &Cite) -> Value {
    match &c.source {
        Source::Url { url, title } => json!({"type": "url_citation", "url_citation": {
            "start_index": c.start, "end_index": c.end, "url": url, "title": title}}),
        Source::Document(d) => json!({"type": "document_citation", "document_citation": {
            "start_index": c.start, "end_index": c.end, "source": d}}),
    }
}

/// A Responses annotation, with offsets relative to the part that starts at `base`.
pub(crate) fn responses_annotation(c: &Cite, base: usize) -> Value {
    let (start, end) = (c.start.saturating_sub(base), c.end.saturating_sub(base));
    match &c.source {
        Source::Url { url, title } => {
            json!({"type": "url_citation", "start_index": start, "end_index": end, "url": url, "title": title})
        }
        Source::Document(d) => {
            json!({"type": "document_citation", "start_index": start, "end_index": end, "source": d})
        }
    }
}

/// A Responses `web_search_call` item.
pub(crate) fn web_search_call(s: &Search) -> Value {
    let mut action = Map::new();
    action.insert("type".into(), "search".into());
    if let Some(q) = &s.query {
        action.insert("query".into(), q.as_str().into());
    }
    json!({"type": "web_search_call", "id": s.id, "status": "completed", "action": action})
}

/// An Anthropic citation of `cited` text.
fn anthropic_citation(source: &Source, cited: &str) -> Value {
    match source {
        Source::Url { url, title } => {
            json!({"type": "web_search_result_location", "url": url, "title": title,
                                             "cited_text": cited, "encrypted_index": ""})
        }
        Source::Document(d) => d.clone(),
    }
}

/// Anthropic text blocks for one text segment that starts at `base`: split where citations begin
/// and end, each cited piece carrying its citations.
pub(crate) fn anthropic_text(text: &str, base: usize, cites: &[Cite]) -> Vec<Value> {
    let len = text.chars().count();
    let local: Vec<&Cite> = cites
        .iter()
        .filter(|c| c.end > base && c.start < base + len && c.end > c.start)
        .collect();
    if local.is_empty() {
        return vec![json!({"type": "text", "text": text})];
    }
    let mut cuts: Vec<usize> = vec![0, len];
    for c in &local {
        cuts.push(c.start.saturating_sub(base).min(len));
        cuts.push(c.end.saturating_sub(base).min(len));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    for w in cuts.windows(2) {
        let (Some(&a), Some(&b)) = (w.first(), w.get(1)) else {
            continue;
        };
        let piece: String = chars.get(a..b).unwrap_or_default().iter().collect();
        if piece.is_empty() {
            continue;
        }
        let citations: Vec<Value> = local
            .iter()
            .filter(|c| c.start <= base + a && c.end >= base + b)
            .map(|c| anthropic_citation(&c.source, &piece))
            .collect();
        if citations.is_empty() {
            out.push(json!({"type": "text", "text": piece}));
        } else {
            out.push(json!({"type": "text", "text": piece, "citations": citations}));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cited_spans_split_anthropic_text() {
        let cites = [Cite {
            start: 4,
            end: 9,
            source: Source::Url {
                url: "https://e.x".into(),
                title: Some("E".into()),
            },
        }];
        let blocks = anthropic_text("See Paris now", 0, &cites);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[1]["text"], "Paris");
        assert_eq!(blocks[1]["citations"][0]["url"], "https://e.x");
        assert_eq!(blocks[1]["citations"][0]["cited_text"], "Paris");
        let a = chat_annotation(&cites[0]);
        assert_eq!(a["url_citation"]["start_index"], 4);
        let (g, s) = from_grounding(
            &json!({"webSearchQueries": ["q"], "groundingChunks": [{"web": {"uri": "https://w", "title": "W"}}],
                    "groundingSupports": [{"segment": {"startIndex": 0, "endIndex": 4}, "groundingChunkIndices": [0]}]}),
            "éabc d",
        );
        assert_eq!((g[0].start, g[0].end), (0, 3));
        assert_eq!(s[0].query.as_deref(), Some("q"));
    }
}

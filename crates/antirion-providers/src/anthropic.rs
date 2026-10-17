//! `anthropic-beta` flags: merged from the client and the provider, and filtered for the platforms
//! that reject flags they do not know.

/// The flags of one or more `anthropic-beta` values: split on `,`, trimmed, empty ones dropped.
#[must_use]
pub fn split_betas<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    values
        .into_iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Client flags first, then the provider's, de-duplicated keeping the first occurrence.
#[must_use]
pub fn merge_betas(client: &[String], provider: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in client.iter().chain(provider) {
        if !out.contains(f) {
            out.push(f.clone());
        }
    }
    out
}

/// A transport that accepts only the flags of its own table.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Platform {
    Bedrock,
    Vertex,
}

/// Client flags the platform accepts, by the name the platform knows them under.
const BEDROCK: &[(&str, &str)] = &[
    ("computer-use-2024-10-22", "computer-use-2024-10-22"),
    ("computer-use-2025-01-24", "computer-use-2025-01-24"),
    (
        "token-efficient-tools-2025-02-19",
        "token-efficient-tools-2025-02-19",
    ),
    (
        "interleaved-thinking-2025-05-14",
        "interleaved-thinking-2025-05-14",
    ),
    ("output-128k-2025-02-19", "output-128k-2025-02-19"),
    ("dev-full-thinking-2025-05-14", "dev-full-thinking-2025-05-14"),
    ("context-1m-2025-08-07", "context-1m-2025-08-07"),
    ("context-management-2025-06-27", "context-management-2025-06-27"),
    ("tool-search-tool-2025-10-19", "tool-search-tool-2025-10-19"),
    ("effort-2025-11-24", "effort-2025-11-24"),
    ("tool-examples-2025-10-29", "tool-examples-2025-10-29"),
    ("advanced-tool-use-2025-11-20", "tool-search-tool-2025-10-19"),
];

const VERTEX: &[(&str, &str)] = &[
    ("computer-use-2024-10-22", "computer-use-2024-10-22"),
    ("computer-use-2025-01-24", "computer-use-2025-01-24"),
    (
        "token-efficient-tools-2025-02-19",
        "token-efficient-tools-2025-02-19",
    ),
    (
        "interleaved-thinking-2025-05-14",
        "interleaved-thinking-2025-05-14",
    ),
    ("output-128k-2025-02-19", "output-128k-2025-02-19"),
    ("context-1m-2025-08-07", "context-1m-2025-08-07"),
    ("context-management-2025-06-27", "context-management-2025-06-27"),
    ("web-search-2025-03-05", "web-search-2025-03-05"),
];

/// The client flags renamed through the platform table; the second value is whether any flag was
/// dropped.
pub(crate) fn filter(platform: Platform, flags: &[String]) -> (Vec<String>, bool) {
    let table = match platform {
        Platform::Bedrock => BEDROCK,
        Platform::Vertex => VERTEX,
    };
    let mut out: Vec<String> = Vec::new();
    let mut dropped = false;
    for f in flags {
        match table.iter().find(|(from, _)| from == f) {
            Some((_, to)) => {
                if !out.iter().any(|o| o == to) {
                    out.push((*to).to_owned());
                }
            }
            None => dropped = true,
        }
    }
    (out, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_keeps_the_first_occurrence() {
        let client = split_betas(["a, b", "", " a"]);
        assert_eq!(client, ["a", "b", "a"]);
        assert_eq!(merge_betas(&client, &["c".into(), "a".into()]), ["a", "b", "c"]);
    }

    #[test]
    fn platform_tables_rename_and_drop() {
        let flags = split_betas(["advanced-tool-use-2025-11-20,unknown-flag,tool-search-tool-2025-10-19"]);
        let (out, dropped) = filter(Platform::Bedrock, &flags);
        assert_eq!(out, ["tool-search-tool-2025-10-19"]);
        assert!(dropped);
        assert_eq!(filter(Platform::Vertex, &[]).0, Vec::<String>::new());
    }
}

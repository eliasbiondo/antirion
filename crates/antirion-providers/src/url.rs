//! Upstream URLs: the base URL's path joined with the operation path, the base query merged with
//! the operation's parameters, and the placeholders filled in.

use crate::sigv4::uri_encode;

/// Substitutes `{name}` placeholders.
pub(crate) fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for (k, v) in vars {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// The model id as one path segment.
pub(crate) fn segment(model: &str) -> String {
    uri_encode(model)
}

/// `base` (scheme, host, path; its query kept) + `path` + the operation's `query`, which replaces
/// same-named base parameters.
pub(crate) fn join(base: &str, path: &str, query: &[(&str, &str)]) -> String {
    let (base, base_query) = match base.split_once('?') {
        Some((b, q)) => (b, q),
        None => (base, ""),
    };
    let mut url = format!("{}{}", base.trim_end_matches('/'), path);
    let mut params: Vec<String> = base_query
        .split('&')
        .filter(|p| !p.is_empty())
        .filter(|p| {
            let name = p.split('=').next().unwrap_or_default();
            !query.iter().any(|(k, _)| *k == name)
        })
        .map(str::to_owned)
        .collect();
    params.extend(query.iter().map(|(k, v)| format!("{k}={}", uri_encode(v))));
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_paths_and_queries() {
        assert_eq!(
            join("https://h/v1/", "/chat/completions", &[]),
            "https://h/v1/chat/completions"
        );
        assert_eq!(
            join(
                "https://h/x?alt=json&k=1",
                "/m:streamGenerateContent",
                &[("alt", "sse")]
            ),
            "https://h/x/m:streamGenerateContent?k=1&alt=sse"
        );
        assert_eq!(segment("anthropic.claude-v1:0"), "anthropic.claude-v1%3A0");
        assert_eq!(segment("arn:aws:x/y"), "arn%3Aaws%3Ax%2Fy");
        assert_eq!(
            fill(
                "https://bedrock-runtime.{region}.amazonaws.com",
                &[("region", "eu-west-1")]
            ),
            "https://bedrock-runtime.eu-west-1.amazonaws.com"
        );
    }
}

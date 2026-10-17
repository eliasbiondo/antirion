//! Namespacing of tool and prompt names (`<prefix>__<name>`) and resource URIs (`<prefix>+<uri>`, and
//! `ui://<prefix>/<rest>` for the `ui` scheme). Prefixes cannot contain `__` or `+`, so splitting at
//! the first separator is unambiguous.

use serde_json::Value;

const NAME_SEP: &str = "__";
const UI_SCHEME: &str = "ui://";

/// The client-facing name of an upstream tool or prompt.
pub(crate) fn name_out(prefix: &str, name: &str) -> String {
    format!("{prefix}{NAME_SEP}{name}")
}

/// `(prefix, upstream name)` of a client-facing name.
pub(crate) fn split_name(name: &str) -> Option<(&str, &str)> {
    name.split_once(NAME_SEP)
}

/// The client-facing form of an upstream URI or URI template.
pub(crate) fn uri_out(prefix: &str, uri: &str) -> String {
    match uri.strip_prefix(UI_SCHEME) {
        Some(rest) => format!("{UI_SCHEME}{prefix}/{rest}"),
        None => format!("{prefix}+{uri}"),
    }
}

/// `(prefix, upstream URI)` of a client-facing URI.
pub(crate) fn split_uri(uri: &str) -> Option<(&str, String)> {
    if let Some(rest) = uri.strip_prefix(UI_SCHEME) {
        let (prefix, tail) = rest.split_once('/')?;
        return Some((prefix, format!("{UI_SCHEME}{tail}")));
    }
    let (prefix, upstream) = uri.split_once('+')?;
    Some((prefix, upstream.to_owned()))
}

fn rewrite_member(obj: &mut Value, key: &str, prefix: &str) {
    if let Some(Value::String(u)) = obj.get_mut(key) {
        *u = uri_out(prefix, u);
    }
}

/// Namespaces the resource URIs a tool result embeds (`resource` and `resource_link` content).
pub(crate) fn tool_result(result: &mut Value, prefix: &str) {
    let Some(items) = result.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("resource") => {
                if let Some(r) = item.get_mut("resource") {
                    rewrite_member(r, "uri", prefix);
                }
            }
            Some("resource_link") => rewrite_member(item, "uri", prefix),
            _ => {}
        }
    }
}

/// Restores the client-facing URIs of a `resources/read` result.
pub(crate) fn read_result(result: &mut Value, prefix: &str) {
    if let Some(items) = result.get_mut("contents").and_then(Value::as_array_mut) {
        for item in items {
            rewrite_member(item, "uri", prefix);
        }
    }
}

/// Namespaces the URI of a `notifications/resources/updated` message.
pub(crate) fn updated_notification(msg: &mut Value, prefix: &str) {
    if let Some(params) = msg.get_mut("params") {
        rewrite_member(params, "uri", prefix);
    }
}

/// Namespaces one list item: `name` for tools and prompts, `uri` or `uriTemplate` for resources.
pub(crate) fn list_item(item: &mut Value, key: &str, prefix: &str) {
    match key {
        "name" => {
            if let Some(Value::String(n)) = item.get_mut("name") {
                *n = name_out(prefix, n);
            }
        }
        other => rewrite_member(item, other, prefix),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn names_split_at_the_first_separator() {
        assert_eq!(split_name("a__get__raw"), Some(("a", "get__raw")));
        assert_eq!(split_name("plain"), None);
        assert_eq!(name_out("github", "create_issue"), "github__create_issue");
    }

    #[test]
    fn uris_round_trip() {
        assert_eq!(uri_out("a", "file:///a.txt"), "a+file:///a.txt");
        assert_eq!(uri_out("b", "ui://widget/x"), "ui://b/widget/x");
        assert_eq!(
            split_uri("ui://b/widget/x"),
            Some(("b", "ui://widget/x".to_owned()))
        );
        assert_eq!(
            split_uri("a+repo://{owner}"),
            Some(("a", "repo://{owner}".to_owned()))
        );
        assert_eq!(split_uri("file:///a.txt"), None);
    }

    #[test]
    fn embedded_resources_are_namespaced_and_text_is_not() {
        let mut r = json!({"content": [
            {"type": "resource", "resource": {"uri": "file:///a.txt", "text": "x"}},
            {"type": "resource_link", "uri": "file:///b.txt"},
            {"type": "text", "text": "file:///a.txt"}]});
        tool_result(&mut r, "a");
        assert_eq!(r["content"][0]["resource"]["uri"], "a+file:///a.txt");
        assert_eq!(r["content"][1]["uri"], "a+file:///b.txt");
        assert_eq!(r["content"][2]["text"], "file:///a.txt");
    }
}

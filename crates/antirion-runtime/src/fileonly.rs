//! What only the configuration file may write: `stdio` servers, which run commands on the node, and
//! `mcp.stdio`, the commands they may run. A change made through the Admin API keeps them as they
//! are; it may remove them.

use antirion_config::{ConfigError, DetailCode, DocPath};
use serde_json::Value;

fn is_stdio(server: &Value) -> bool {
    server.get("transport").and_then(Value::as_str) == Some("stdio")
}

/// The file-only settings `candidate` adds or changes compared with `previous`.
pub(crate) fn check(previous: &Value, candidate: &Value) -> Vec<ConfigError> {
    let mut errors = Vec::new();
    let mcp = DocPath::root().key("mcp");
    if let Some(stdio) = candidate.pointer("/mcp/stdio")
        && previous.pointer("/mcp/stdio") != Some(stdio)
    {
        errors.push(ConfigError::new(
            &mcp.key("stdio"),
            DetailCode::StaticSetting,
            "mcp.stdio is set in the configuration file only",
        ));
    }
    for (name, server) in candidate
        .pointer("/mcp/servers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let before = previous.pointer("/mcp/servers").and_then(|s| s.get(name));
        let changed = before != Some(server);
        if changed && (is_stdio(server) || before.is_some_and(is_stdio)) {
            errors.push(ConfigError::new(
                &mcp.key("servers").key(name),
                DetailCode::StaticSetting,
                "stdio servers are defined in the configuration file only",
            ));
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn stdio_servers_can_be_kept_or_removed_but_not_written() {
        let file = json!({"mcp": {"stdio": {"allowed_commands": ["/bin/x"]},
                          "servers": {"s": {"transport": "stdio", "command": "/bin/x"}}}});
        assert!(check(&file, &file).is_empty());
        assert!(check(&file, &json!({})).is_empty(), "removal is allowed");
        let changed = json!({"mcp": {"stdio": {"allowed_commands": ["/bin/x"]},
                             "servers": {"s": {"transport": "stdio", "command": "/bin/y"}}}});
        assert_eq!(check(&file, &changed)[0].path, "mcp.servers.s");
        let added = json!({"mcp": {"stdio": {"allowed_commands": ["/bin/x", "/bin/z"]}}});
        assert_eq!(check(&file, &added)[0].path, "mcp.stdio");
        let http =
            json!({"mcp": {"servers": {"h": {"transport": "streamable_http", "url": "https://h/mcp"}}}});
        assert!(check(&json!({}), &http).is_empty());
    }
}

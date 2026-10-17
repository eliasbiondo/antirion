//! Anthropic-defined client tools as function tools: the input schemas Anthropic publishes for its
//! bash and text editor tools, for the deployments of other dialects, which take function tools.

use serde_json::{Value, json};

/// The function form of an Anthropic-defined client tool of type `kind`: its default name, a
/// description and the input schema. `None` for types without a function form.
#[must_use]
pub fn function_form(kind: &str) -> Option<(&'static str, &'static str, Value)> {
    if kind.starts_with("bash_") {
        return Some((
            "bash",
            "Run commands in a bash shell. The session persists between calls.",
            json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The bash command to run. Required unless the tool is being restarted."},
                    "restart": {"type": "boolean", "description": "Specifying true will restart this tool. Otherwise, leave this unspecified."}
                }
            }),
        ));
    }
    if kind.starts_with("text_editor_") {
        let name = if kind == "text_editor_20250124" || kind == "text_editor_20241022" {
            "str_replace_editor"
        } else {
            "str_replace_based_edit_tool"
        };
        return Some((
            name,
            "View, create and edit text files: view, create, str_replace and insert commands on a path.",
            json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "enum": ["view", "create", "str_replace", "insert", "undo_edit"],
                                "description": "The command to run."},
                    "path": {"type": "string", "description": "Absolute path to the file or directory."},
                    "file_text": {"type": "string", "description": "The content of the file to create (create)."},
                    "old_str": {"type": "string", "description": "The text to replace, which must match exactly once (str_replace)."},
                    "new_str": {"type": "string", "description": "The new text (str_replace, insert)."},
                    "insert_line": {"type": "integer", "description": "The line after which to insert the text (insert)."},
                    "view_range": {"type": "array", "items": {"type": "integer"},
                                   "description": "The first and last line to view; -1 as the last means the end of the file (view)."}
                },
                "required": ["command", "path"]
            }),
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_and_editor_have_function_forms() {
        assert_eq!(function_form("bash_20250124").map(|f| f.0), Some("bash"));
        assert_eq!(
            function_form("text_editor_20250728").map(|f| f.0),
            Some("str_replace_based_edit_tool")
        );
        assert_eq!(
            function_form("text_editor_20250124").map(|f| f.0),
            Some("str_replace_editor")
        );
        assert!(function_form("computer_20250124").is_none());
    }
}

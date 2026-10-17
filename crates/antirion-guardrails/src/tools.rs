//! The `tool_permission` check: which tool names a request may declare and a response may call,
//! by exact name or `*` glob. At input a refused declaration is a finding (or, under `redact`, is
//! removed from the request); at output a call to a refused tool is a finding.

use serde_json::Value;

use crate::segments::Dialect;

/// `allow` and `deny` lists of tool-name patterns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolRules {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

/// Whether `name` matches `pattern`, where `*` matches any run of characters.
#[must_use]
pub fn glob(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let tail: Vec<&str> = parts.collect();
    let Some((last, middle)) = tail.split_last() else {
        return rest.is_empty();
    };
    for p in middle {
        match rest.find(p) {
            Some(i) => rest = rest.get(i + p.len()..).unwrap_or_default(),
            None => return false,
        }
    }
    rest.ends_with(last)
}

impl ToolRules {
    pub(crate) fn from_block(block: Option<&Value>) -> ToolRules {
        let list = |key: &str| -> Vec<String> {
            block
                .and_then(|b| b.get(key))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        };
        ToolRules {
            allow: list("allow"),
            deny: list("deny"),
        }
    }

    /// Whether a tool named `name` is refused: denied, or not allowed when `allow` is set.
    #[must_use]
    pub fn refuses(&self, name: &str) -> bool {
        self.deny.iter().any(|p| glob(p, name))
            || (!self.allow.is_empty() && !self.allow.iter().any(|p| glob(p, name)))
    }

    /// The first refused tool the request declares.
    #[must_use]
    pub fn refused_declaration(&self, body: &Value, d: Dialect) -> Option<String> {
        declared(body, d).into_iter().find(|n| self.refuses(n))
    }

    /// The first refused tool the response calls.
    #[must_use]
    pub fn refused_call(&self, body: &Value, d: Dialect) -> Option<String> {
        called(body, d).into_iter().find(|n| self.refuses(n))
    }

    /// Removes the refused tools from the request's tool list; the names removed. A list left
    /// empty goes, with the tool choice; a tool choice naming a removed tool goes too.
    pub fn remove_refused(&self, body: &mut Value, d: Dialect) -> Vec<String> {
        let mut removed = Vec::new();
        let (list_ptr, name_of): (&str, fn(&Value) -> Option<&str>) = match d {
            Dialect::Chat => ("/tools", |t| t.pointer("/function/name").and_then(Value::as_str)),
            Dialect::Responses | Dialect::Anthropic => ("/tools", |t| t.get("name").and_then(Value::as_str)),
            Dialect::Converse => ("/toolConfig/tools", |t| {
                t.pointer("/toolSpec/name").and_then(Value::as_str)
            }),
            Dialect::Gemini => {
                for decl in body
                    .get_mut("tools")
                    .and_then(Value::as_array_mut)
                    .into_iter()
                    .flatten()
                {
                    if let Some(fs) = decl.get_mut("functionDeclarations").and_then(Value::as_array_mut) {
                        fs.retain(|f| match f.get("name").and_then(Value::as_str) {
                            Some(n) if self.refuses(n) => {
                                removed.push(n.to_owned());
                                false
                            }
                            _ => true,
                        });
                    }
                }
                return removed;
            }
            Dialect::Completions
            | Dialect::Embeddings
            | Dialect::Images
            | Dialect::Rerank
            | Dialect::Transcription => return removed,
        };
        let Some(list) = body.pointer_mut(list_ptr).and_then(Value::as_array_mut) else {
            return removed;
        };
        list.retain(|t| match name_of(t) {
            Some(n) if self.refuses(n) => {
                removed.push(n.to_owned());
                false
            }
            _ => true,
        });
        let emptied = list.is_empty();
        if removed.is_empty() {
            return removed;
        }
        let choice = body.get("tool_choice").map(chosen_name);
        let drop_choice = emptied || choice.flatten().is_some_and(|n| removed.iter().any(|r| r == n));
        if let Some(o) = body.as_object_mut() {
            if emptied && d != Dialect::Converse {
                o.shift_remove("tools");
            }
            if drop_choice {
                o.shift_remove("tool_choice");
            }
        }
        if emptied
            && d == Dialect::Converse
            && let Some(o) = body.as_object_mut()
        {
            o.shift_remove("toolConfig");
        }
        removed
    }
}

/// The tool a `tool_choice` names, in any dialect.
fn chosen_name(choice: &Value) -> Option<&str> {
    choice
        .pointer("/function/name")
        .or_else(|| choice.get("name"))
        .and_then(Value::as_str)
}

fn names<'a>(items: Option<&'a Value>, name: impl Fn(&'a Value) -> Option<&'a str>) -> Vec<String> {
    items
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(name)
        .map(str::to_owned)
        .collect()
}

/// The tool names a request declares.
#[must_use]
pub fn declared(body: &Value, d: Dialect) -> Vec<String> {
    match d {
        Dialect::Chat => names(body.get("tools"), |t| {
            t.pointer("/function/name").and_then(Value::as_str)
        }),
        Dialect::Responses | Dialect::Anthropic => {
            names(body.get("tools"), |t| t.get("name").and_then(Value::as_str))
        }
        Dialect::Converse => names(body.pointer("/toolConfig/tools"), |t| {
            t.pointer("/toolSpec/name").and_then(Value::as_str)
        }),
        Dialect::Gemini => body
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|t| {
                names(t.get("functionDeclarations"), |f| {
                    f.get("name").and_then(Value::as_str)
                })
            })
            .collect(),
        Dialect::Completions
        | Dialect::Embeddings
        | Dialect::Images
        | Dialect::Rerank
        | Dialect::Transcription => Vec::new(),
    }
}

/// The tool names a response calls, over every choice.
#[must_use]
pub fn called(body: &Value, d: Dialect) -> Vec<String> {
    let arr = |v: Option<&Value>| v.and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out = Vec::new();
    match d {
        Dialect::Chat => {
            for c in arr(body.get("choices")) {
                out.extend(names(c.pointer("/message/tool_calls"), |t| {
                    t.pointer("/function/name").and_then(Value::as_str)
                }));
            }
        }
        Dialect::Anthropic => out.extend(names(body.get("content"), |b| {
            (b.get("type").and_then(Value::as_str) == Some("tool_use"))
                .then(|| b.get("name").and_then(Value::as_str))
                .flatten()
        })),
        Dialect::Responses => out.extend(names(body.get("output"), |i| {
            (i.get("type").and_then(Value::as_str) == Some("function_call"))
                .then(|| i.get("name").and_then(Value::as_str))
                .flatten()
        })),
        Dialect::Gemini => {
            for c in arr(body.get("candidates")) {
                out.extend(names(c.pointer("/content/parts"), |p| {
                    p.pointer("/functionCall/name").and_then(Value::as_str)
                }));
            }
        }
        Dialect::Converse => out.extend(names(body.pointer("/output/message/content"), |b| {
            b.pointer("/toolUse/name").and_then(Value::as_str)
        })),
        Dialect::Completions
        | Dialect::Embeddings
        | Dialect::Images
        | Dialect::Rerank
        | Dialect::Transcription => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn globs_match_whole_names() {
        assert!(glob("search_web", "search_web"));
        assert!(!glob("search_web", "search_web2"));
        assert!(glob("search_*", "search_web"));
        assert!(glob("*_web", "search_web"));
        assert!(glob("s*_*b", "search_web"));
        assert!(!glob("s*_*x", "search_web"));
        assert!(glob("*", ""));
    }

    #[test]
    fn allow_and_deny_lists() {
        let r = ToolRules {
            allow: vec!["search_*".into()],
            deny: vec!["search_secret".into()],
        };
        assert!(!r.refuses("search_web"));
        assert!(r.refuses("search_secret"));
        assert!(r.refuses("exec_cmd"));
        assert!(!ToolRules::default().refuses("anything"));
    }

    #[test]
    fn refused_tools_are_found_and_removed() {
        let r = ToolRules {
            allow: vec!["search_web".into()],
            deny: Vec::new(),
        };
        let mut chat = json!({"tools": [
            {"type": "function", "function": {"name": "search_web"}},
            {"type": "function", "function": {"name": "exec_cmd"}}],
            "tool_choice": {"type": "function", "function": {"name": "exec_cmd"}}});
        assert_eq!(
            r.refused_declaration(&chat, Dialect::Chat).as_deref(),
            Some("exec_cmd")
        );
        assert_eq!(r.remove_refused(&mut chat, Dialect::Chat), ["exec_cmd"]);
        assert_eq!(
            chat,
            json!({"tools": [{"type": "function", "function": {"name": "search_web"}}]})
        );
        let mut anthropic = json!({"tools": [{"name": "exec_cmd"}], "tool_choice": {"type": "auto"}});
        r.remove_refused(&mut anthropic, Dialect::Anthropic);
        assert_eq!(anthropic, json!({}));
        let out = json!({"choices": [{"message": {"tool_calls": [{"function": {"name": "exec_cmd"}}]}}]});
        assert_eq!(r.refused_call(&out, Dialect::Chat).as_deref(), Some("exec_cmd"));
        let gemini = json!({"tools": [{"functionDeclarations": [{"name": "search_web"}, {"name": "x"}]}]});
        assert_eq!(declared(&gemini, Dialect::Gemini), ["search_web", "x"]);
    }
}

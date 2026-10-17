//! Content transforms: forced system prompts and prompt decorators, applied to the client body per
//! dialect. Inserted objects carry the operator mark; strings written whole are reported by pointer.

use serde_json::{Value, json};

use crate::def::{Decoration, Decorator, PromptMode, SystemPrompt};
use crate::segments::{Dialect, OPERATOR_MARK};

fn marked(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.insert(OPERATOR_MARK.into(), Value::Bool(true));
    }
    v
}

fn messages<'a>(body: &'a mut Value, key: &str) -> Option<&'a mut Vec<Value>> {
    let o = body.as_object_mut()?;
    if !o.get(key).is_some_and(Value::is_array) {
        o.insert(key.into(), Value::Array(Vec::new()));
    }
    o.get_mut(key).and_then(Value::as_array_mut)
}

fn is_system(m: &Value) -> bool {
    matches!(
        m.get("role").and_then(Value::as_str),
        Some("system" | "developer")
    )
}

fn joined(a: &str, b: &str) -> String {
    format!("{a}\n\n{b}")
}

/// Applies a `system_prompt` transform; returns the pointers of strings it wrote whole.
pub fn system_prompt(body: &mut Value, d: Dialect, sp: &SystemPrompt) -> Vec<String> {
    match d {
        Dialect::Chat => {
            chat_prompt(body, sp);
            Vec::new()
        }
        Dialect::Anthropic => top_level(body, "system", sp, true),
        Dialect::Responses => top_level(body, "instructions", sp, false),
        _ => Vec::new(),
    }
}

fn chat_prompt(body: &mut Value, sp: &SystemPrompt) {
    let Some(msgs) = messages(body, "messages") else {
        return;
    };
    let fresh = || marked(json!({"role": sp.role, "content": sp.text}));
    let first = msgs.iter().position(is_system);
    match (sp.mode, first) {
        (PromptMode::Prepend, _)
        | (PromptMode::Replace | PromptMode::Append | PromptMode::InsertIfAbsent, None) => {
            msgs.insert(0, fresh());
        }
        (PromptMode::InsertIfAbsent, Some(_)) => {}
        (PromptMode::Replace, Some(i)) => {
            if let Some(Value::Object(m)) = msgs.get_mut(i) {
                m.insert("content".into(), Value::String(sp.text.clone()));
                m.insert(OPERATOR_MARK.into(), Value::Bool(true));
            }
        }
        (PromptMode::Append, Some(i)) => {
            let Some(m) = msgs.get_mut(i).and_then(Value::as_object_mut) else {
                return;
            };
            match m.get_mut("content") {
                Some(Value::String(s)) => *s = joined(s, &sp.text),
                Some(Value::Array(parts)) => {
                    parts.push(marked(
                        json!({"type": "text", "text": format!("\n\n{}", sp.text)}),
                    ));
                }
                _ => {
                    m.insert("content".into(), Value::String(sp.text.clone()));
                }
            }
        }
    }
}

/// A prompt carried in one top-level member (`system`, `instructions`); `blocks` when it may be an
/// array of text blocks.
fn top_level(body: &mut Value, key: &str, sp: &SystemPrompt, blocks: bool) -> Vec<String> {
    let Some(o) = body.as_object_mut() else {
        return Vec::new();
    };
    let absent = o.get(key).is_none_or(Value::is_null);
    if sp.mode == PromptMode::Replace || absent {
        o.insert(key.into(), Value::String(sp.text.clone()));
        return vec![format!("/{key}")];
    }
    let block = || marked(json!({"type": "text", "text": sp.text}));
    match (sp.mode, o.get_mut(key)) {
        (PromptMode::Prepend, Some(Value::String(s))) => {
            *s = joined(&sp.text, s);
            Vec::new()
        }
        (PromptMode::Append, Some(Value::String(s))) => {
            *s = joined(s, &sp.text);
            Vec::new()
        }
        (PromptMode::Prepend, Some(Value::Array(a))) if blocks => {
            a.insert(0, block());
            Vec::new()
        }
        (PromptMode::Append, Some(Value::Array(a))) if blocks => {
            a.push(block());
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn item(d: &Decoration) -> Value {
    marked(json!({"role": d.role, "content": d.content}))
}

/// The text parts of a message's content: the string itself, or the parts of the given types.
fn texts_of<'a>(content: &'a mut Value, part_types: &[&str]) -> Vec<&'a mut String> {
    match content {
        Value::String(s) => vec![s],
        Value::Array(parts) => parts
            .iter_mut()
            .filter(|p| {
                p.get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|t| part_types.contains(&t))
            })
            .filter_map(|p| match p.get_mut("text") {
                Some(Value::String(s)) => Some(s),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Adds `prefix` to the first text and `suffix` to the last text of the last user message.
fn affix(body: &mut Value, d: Dialect, prefix: Option<&str>, suffix: Option<&str>) {
    if prefix.is_none() && suffix.is_none() {
        return;
    }
    let (list_key, part_types): (&str, &[&str]) = match d {
        Dialect::Chat | Dialect::Anthropic => ("messages", &["text"]),
        Dialect::Responses => ("input", &["input_text", "text"]),
        _ => return,
    };
    let Some(target) = body.get_mut(list_key) else {
        return;
    };
    let content = match target {
        Value::String(_) if d == Dialect::Responses => Some(target),
        Value::Array(items) => items
            .iter_mut()
            .rev()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
            .and_then(|m| m.get_mut("content")),
        _ => None,
    };
    let Some(content) = content else { return };
    let mut texts = texts_of(content, part_types);
    if let (Some(p), Some(first)) = (prefix, texts.first_mut()) {
        first.insert_str(0, p);
    }
    if let (Some(s), Some(last)) = (suffix, texts.last_mut()) {
        last.push_str(s);
    }
}

/// Applies a `decorator` transform; returns the pointers of strings it wrote whole.
pub fn decorate(body: &mut Value, d: Dialect, dec: &Decorator) -> Vec<String> {
    affix(body, d, dec.user_prefix.as_deref(), dec.user_suffix.as_deref());
    match d {
        Dialect::Chat => {
            if let Some(msgs) = messages(body, "messages") {
                wrap(msgs, &dec.prepend, &dec.append);
            }
            Vec::new()
        }
        Dialect::Responses => {
            let Some(o) = body.as_object_mut() else {
                return Vec::new();
            };
            if let Some(Value::String(s)) = o.get("input").cloned() {
                o.insert("input".into(), json!([{"role": "user", "content": s}]));
            }
            if let Some(items) = messages(body, "input") {
                wrap(items, &dec.prepend, &dec.append);
            }
            Vec::new()
        }
        Dialect::Anthropic => anthropic(body, dec),
        _ => Vec::new(),
    }
}

fn wrap(list: &mut Vec<Value>, prepend: &[Decoration], append: &[Decoration]) {
    for (i, d) in prepend.iter().enumerate() {
        list.insert(i, item(d));
    }
    list.extend(append.iter().map(item));
}

fn anthropic(body: &mut Value, dec: &Decorator) -> Vec<String> {
    let system = |list: &[Decoration]| -> Vec<String> {
        list.iter()
            .filter(|d| matches!(d.role.as_str(), "system" | "developer"))
            .map(|d| d.content.clone())
            .collect()
    };
    let others = |list: &[Decoration]| -> Vec<Decoration> {
        list.iter()
            .filter(|d| !matches!(d.role.as_str(), "system" | "developer"))
            .cloned()
            .collect()
    };
    let (pre_sys, post_sys) = (system(&dec.prepend), system(&dec.append));
    let mut whole = Vec::new();
    if let Some(o) = body.as_object_mut() {
        let text_block = |t: &str| marked(json!({"type": "text", "text": t}));
        match o.get_mut("system") {
            Some(Value::String(s)) => {
                let mut parts: Vec<String> = pre_sys.clone();
                parts.push(s.clone());
                parts.extend(post_sys.iter().cloned());
                *s = parts.join("\n\n");
            }
            Some(Value::Array(a)) => {
                for (i, t) in pre_sys.iter().enumerate() {
                    a.insert(i, text_block(t));
                }
                a.extend(post_sys.iter().map(|t| text_block(t)));
            }
            _ => {
                let all: Vec<String> = pre_sys.iter().chain(&post_sys).cloned().collect();
                if !all.is_empty() {
                    o.insert("system".into(), Value::String(all.join("\n\n")));
                    whole.push("/system".to_owned());
                }
            }
        }
    }
    if let Some(msgs) = messages(body, "messages") {
        wrap(msgs, &others(&dec.prepend), &others(&dec.append));
    }
    whole
}

/// A decoration list from config.
pub(crate) fn decorations(v: Option<&Value>) -> Vec<Decoration> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|d| {
            Some(Decoration {
                role: d.get("role")?.as_str()?.to_owned(),
                content: d.get("content")?.as_str()?.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segments::strip_marks;

    fn sp(mode: PromptMode) -> SystemPrompt {
        SystemPrompt {
            text: "NEW".into(),
            mode,
            role: "system".into(),
        }
    }

    fn chat(mode: PromptMode, msgs: Value) -> Value {
        let mut body = json!({"messages": msgs});
        system_prompt(&mut body, Dialect::Chat, &sp(mode));
        strip_marks(&mut body);
        body["messages"].clone()
    }

    #[test]
    fn chat_modes() {
        let u = json!({"role": "user", "content": "hi"});
        let old = json!({"role": "system", "content": "old"});
        assert_eq!(
            chat(PromptMode::Replace, json!([old, u])),
            json!([{"role": "system", "content": "NEW"}, u])
        );
        assert_eq!(
            chat(PromptMode::Prepend, json!([old, u])),
            json!([{"role": "system", "content": "NEW"}, old, u])
        );
        assert_eq!(
            chat(PromptMode::Append, json!([old, u])),
            json!([{"role": "system", "content": "old\n\nNEW"}, u])
        );
        assert_eq!(chat(PromptMode::InsertIfAbsent, json!([old, u])), json!([old, u]));
        assert_eq!(
            chat(PromptMode::Append, json!([u])),
            json!([{"role": "system", "content": "NEW"}, u])
        );
    }

    #[test]
    fn anthropic_modes() {
        let run = |mode, system: Option<Value>| {
            let mut body = json!({"messages": []});
            if let Some(s) = system {
                body["system"] = s;
            }
            system_prompt(&mut body, Dialect::Anthropic, &sp(mode));
            strip_marks(&mut body);
            body["system"].clone()
        };
        assert_eq!(run(PromptMode::Prepend, Some(json!("old"))), json!("NEW\n\nold"));
        assert_eq!(run(PromptMode::Prepend, None), json!("NEW"));
        assert_eq!(run(PromptMode::Append, None), json!("NEW"));
        assert_eq!(run(PromptMode::InsertIfAbsent, Some(json!("old"))), json!("old"));
        assert_eq!(
            run(PromptMode::Append, Some(json!([{"type": "text", "text": "old"}]))),
            json!([{"type": "text", "text": "old"}, {"type": "text", "text": "NEW"}])
        );
    }

    #[test]
    fn anthropic_decorator_moves_system_items() {
        let dec = Decorator {
            prepend: vec![
                Decoration {
                    role: "system".into(),
                    content: "P1".into(),
                },
                Decoration {
                    role: "user".into(),
                    content: "P2".into(),
                },
            ],
            append: vec![Decoration {
                role: "user".into(),
                content: "A1".into(),
            }],
            ..Decorator::default()
        };
        let mut body = json!({"system": "client", "messages": [{"role": "user", "content": "hi"}]});
        decorate(&mut body, Dialect::Anthropic, &dec);
        strip_marks(&mut body);
        assert_eq!(body["system"], "P1\n\nclient");
        assert_eq!(
            body["messages"],
            json!([{"role": "user", "content": "P2"}, {"role": "user", "content": "hi"}, {"role": "user", "content": "A1"}])
        );
    }

    #[test]
    fn affixes_wrap_the_last_user_message() {
        let dec = Decorator {
            user_prefix: Some("SYS: ".into()),
            user_suffix: Some(" (be brief)".into()),
            ..Decorator::default()
        };
        let mut body = json!({"messages": [{"role": "user", "content": "first"}, {"role": "assistant", "content": "ok"},
            {"role": "user", "content": [{"type": "text", "text": "a"}, {"type": "image_url", "image_url": {"url": "u"}},
                                         {"type": "text", "text": "b"}]}]});
        decorate(&mut body, Dialect::Chat, &dec);
        assert_eq!(body["messages"][0]["content"], "first");
        assert_eq!(body["messages"][2]["content"][0]["text"], "SYS: a");
        assert_eq!(body["messages"][2]["content"][2]["text"], "b (be brief)");
        let mut body = json!({"input": "hi"});
        decorate(&mut body, Dialect::Responses, &dec);
        strip_marks(&mut body);
        assert_eq!(
            body["input"],
            json!([{"role": "user", "content": "SYS: hi (be brief)"}])
        );
    }
}

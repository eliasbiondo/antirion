//! Completions, Embeddings, image generation, speech, Responses and Anthropic Messages.

use antirion_json::Skim;
use serde_json::Value;

use super::{
    Check, Table, ValidateOpts, Violation, array, bad_value, boolean, index, integer_in, key, member_value,
    missing, non_empty_string, number_in, object, string, wrong_type,
};

pub(crate) static COMPLETIONS: Table = Table {
    member: completions,
    required: &["prompt"],
    finish: None,
    on_error: None,
};

pub(crate) static EMBEDDINGS: Table = Table {
    member: embeddings,
    required: &["input"],
    finish: None,
    on_error: None,
};

pub(crate) static IMAGES: Table = Table {
    member: images,
    required: &["prompt"],
    finish: None,
    on_error: None,
};

pub(crate) static SPEECH: Table = Table {
    member: speech,
    required: &["input", "voice"],
    finish: None,
    on_error: None,
};

pub(crate) static MODERATIONS: Table = Table {
    member: moderations,
    required: &["input"],
    finish: None,
    on_error: None,
};

pub(crate) static RESPONSES: Table = Table {
    member: responses,
    required: &[],
    finish: Some(responses_source),
    on_error: None,
};

pub(crate) static MESSAGES: Table = Table {
    member: messages_member,
    required: &["max_tokens", "messages"],
    finish: None,
    on_error: Some(openai_hint),
};

/// `count_tokens` takes a Messages body without `max_tokens`.
pub(crate) static COUNT_TOKENS: Table = Table {
    member: messages_member,
    required: &["messages"],
    finish: None,
    on_error: Some(openai_hint),
};

fn completions(name: &str, v: &Value, _opts: &ValidateOpts) -> Check {
    match name {
        "prompt" => prompt(v),
        "stream" if !v.is_null() => boolean(v, name),
        _ => Ok(()),
    }
}

/// A string, an array of strings, an array of integers, or an array of integer arrays.
fn prompt(v: &Value) -> Check {
    let expected = "a string, an array of strings, an array of integers or an array of integer arrays";
    match v {
        Value::String(_) => Ok(()),
        Value::Array(items) => match items.first() {
            None | Some(Value::String(_)) => each(items, "prompt", |s, p| string(s, p).map(|_| ())),
            Some(Value::Array(_)) => each(items, "prompt", |t, p| tokens(t, p, false)),
            Some(_) => each(items, "prompt", token),
        },
        _ => Err(wrong_type("prompt", expected)),
    }
}

fn embeddings(name: &str, v: &Value, _opts: &ValidateOpts) -> Check {
    if name != "input" {
        return Ok(());
    }
    match v {
        Value::String(_) => non_empty_string(v, "input"),
        Value::Array(items) => {
            if items.is_empty() {
                return Err(bad_value("input", "must not be empty."));
            }
            match items.first() {
                Some(Value::String(_)) => each(items, "input", non_empty_string),
                Some(Value::Array(_)) => each(items, "input", |t, p| tokens(t, p, true)),
                _ => each(items, "input", token),
            }
        }
        _ => Err(wrong_type(
            "input",
            "a string, an array of strings, an array of integers or an array of integer arrays",
        )),
    }
}

fn each(items: &[Value], parent: &str, f: impl Fn(&Value, &str) -> Check) -> Check {
    for (i, item) in items.iter().enumerate() {
        f(item, &index(parent, i))?;
    }
    Ok(())
}

fn token(v: &Value, path: &str) -> Check {
    integer_in(v, path, i64::MIN, None)
}

fn tokens(v: &Value, path: &str, non_empty: bool) -> Check {
    let list = array(v, path)?;
    if non_empty && list.is_empty() {
        return Err(bad_value(path, "must not be empty."));
    }
    each(list, path, token)
}

fn images(name: &str, v: &Value, opts: &ValidateOpts) -> Check {
    match name {
        "prompt" => non_empty_string(v, name),
        "n" if !v.is_null() => {
            let max = i64::try_from(opts.images_max_n).unwrap_or(i64::MAX);
            integer_in(v, name, 1, Some(max))
        }
        _ => Ok(()),
    }
}

fn moderations(name: &str, v: &Value, _opts: &ValidateOpts) -> Check {
    match name {
        "input" => match v {
            Value::String(_) | Value::Array(_) => Ok(()),
            _ => Err(wrong_type(name, "a string or an array")),
        },
        _ => Ok(()),
    }
}

fn speech(name: &str, v: &Value, _opts: &ValidateOpts) -> Check {
    match name {
        "input" => non_empty_string(v, name),
        "voice" => match v {
            Value::String(_) => non_empty_string(v, name),
            Value::Object(_) => Ok(()),
            _ => Err(wrong_type(name, "a string or an object")),
        },
        _ => Ok(()),
    }
}

fn responses(name: &str, v: &Value, _opts: &ValidateOpts) -> Check {
    match name {
        "input" if !v.is_null() => match v {
            Value::String(_) | Value::Array(_) => Ok(()),
            _ => Err(wrong_type(name, "a string or an array of items")),
        },
        "stream" if !v.is_null() => boolean(v, name),
        _ => Ok(()),
    }
}

/// A Responses request needs something to answer: `input`, or a stored conversation to continue.
fn responses_source(skim: &Skim) -> Check {
    if ["input", "previous_response_id", "conversation"]
        .iter()
        .any(|m| skim.member(m).is_some())
    {
        return Ok(());
    }
    Err(missing("input"))
}

fn messages_member(name: &str, v: &Value, _opts: &ValidateOpts) -> Check {
    match name {
        "max_tokens" => integer_in(v, name, 0, None),
        "messages" => anthropic_messages(v),
        "temperature" if !v.is_null() => number_in(v, name, 0.0, 1.0),
        "top_p" if !v.is_null() => number_in(v, name, 0.0, 1.0),
        "stream" if !v.is_null() => boolean(v, name),
        "thinking" if !v.is_null() => thinking(v),
        _ => Ok(()),
    }
}

fn anthropic_messages(v: &Value) -> Check {
    let list = array(v, "messages")?;
    if list.is_empty() {
        return Err(bad_value("messages", "must contain at least one message."));
    }
    for (i, m) in list.iter().enumerate() {
        let path = index("messages", i);
        let msg = object(m, &path)?;
        let role_path = key(&path, "role");
        match msg.get("role") {
            None => return Err(missing(&role_path)),
            Some(r) => {
                if !["user", "assistant", "system"].contains(&string(r, &role_path)?) {
                    return Err(bad_value(&role_path, "must be one of user, assistant, system."));
                }
            }
        }
        let content_path = key(&path, "content");
        match msg.get("content") {
            None => return Err(missing(&content_path)),
            Some(Value::String(_)) => {}
            Some(Value::Array(blocks)) => {
                for (j, b) in blocks.iter().enumerate() {
                    let block_path = index(&content_path, j);
                    let block = object(b, &block_path)?;
                    let type_path = key(&block_path, "type");
                    match block.get("type") {
                        None => return Err(missing(&type_path)),
                        Some(t) => {
                            string(t, &type_path)?;
                        }
                    }
                }
            }
            Some(_) => {
                return Err(wrong_type(
                    &content_path,
                    "a string or an array of content blocks",
                ));
            }
        }
    }
    Ok(())
}

fn thinking(v: &Value) -> Check {
    let t = object(v, "thinking")?;
    if t.get("type").and_then(Value::as_str) != Some("enabled") {
        return Ok(());
    }
    match t.get("budget_tokens") {
        None => Err(missing("thinking.budget_tokens")),
        Some(b) => integer_in(b, "thinking.budget_tokens", 1024, None),
    }
}

/// Bodies shaped for Chat Completions sent to Messages get a pointer to the right endpoint.
fn openai_hint(v: Violation, raw: &[u8], skim: &Skim) -> Violation {
    let system_first = skim
        .member("messages")
        .and_then(|m| member_value(raw, &m.value))
        .and_then(|msgs| {
            msgs.as_array()
                .and_then(|a| a.first())
                .and_then(|m| m.get("role"))
                .and_then(Value::as_str)
                .map(|r| r == "system")
        })
        .unwrap_or(false);
    if skim.member("max_completion_tokens").is_none() && !system_first {
        return v;
    }
    Violation {
        message: format!(
            "{} Hint: this looks like an OpenAI Chat Completions body; send it to /v1/chat/completions.",
            v.message
        ),
        ..v
    }
}

#[cfg(test)]
mod tests {
    use super::super::ValidateOpts;
    use super::super::ViolationKind::{Missing, Type, Value as Val};
    use super::super::tests::{check_at, rejects};
    use crate::endpoints::{Matched, match_endpoint};

    const EMB: &str = "/v1/embeddings";
    const IMG: &str = "/v1/images/generations";
    const SPEECH: &str = "/v1/audio/speech";
    const RESP: &str = "/v1/responses";
    const MSG: &str = "/v1/messages";

    #[test]
    fn embeddings_inputs() {
        for ok in [r#""hi""#, r#"["a","b"]"#, "[1,2,3]", "[[1,2],[3]]"] {
            let b = format!(r#"{{"model":"m","input":{ok},"dimensions":256,"encoding_format":"base64"}}"#);
            assert_eq!(check_at(EMB, &b), Ok(()), "{b}");
        }
        for (input, param) in [
            ("[]", "input"),
            (r#""""#, "input"),
            (r#"["a",""]"#, "input[1]"),
            ("[[]]", "input[0]"),
        ] {
            rejects(
                EMB,
                &format!(r#"{{"model":"m","input":{input}}}"#),
                param,
                Some(Val),
            );
        }
        rejects(EMB, r#"{"model":"m"}"#, "input", Some(Missing));
        rejects(EMB, r#"{"model":"m","input":5}"#, "input", Some(Type));
        rejects(EMB, r#"{"model":"m","input":["a",5]}"#, "input[1]", Some(Type));
        rejects(
            EMB,
            r#"{"model":"m","input":[[1],["x"]]}"#,
            "input[1][0]",
            Some(Type),
        );
    }

    #[test]
    fn images() {
        rejects(IMG, "{}", "prompt", Some(Missing));
        rejects(IMG, r#"{"model":"m"}"#, "prompt", Some(Missing));
        rejects(IMG, r#"{"model":"m","prompt":""}"#, "prompt", Some(Val));
        rejects(IMG, r#"{"model":"m","prompt":5}"#, "prompt", Some(Type));
        for (n, kind) in [
            ("0", Some(Val)),
            ("11", Some(Val)),
            ("2.5", None),
            (r#""2""#, None),
        ] {
            rejects(
                IMG,
                &format!(r#"{{"model":"m","prompt":"a cat","n":{n}}}"#),
                "n",
                kind,
            );
        }
        for n in ["1", "10"] {
            assert_eq!(
                check_at(IMG, &format!(r#"{{"model":"m","prompt":"a cat","n":{n}}}"#)),
                Ok(())
            );
        }
    }

    #[test]
    fn images_n_bound_follows_the_option() {
        let Matched::Found(e) = match_endpoint(&http::Method::POST, IMG) else {
            panic!()
        };
        let raw = br#"{"model":"m","prompt":"a cat","n":3}"#;
        let s = antirion_json::skim(raw, &antirion_json::JsonLimits::default()).unwrap();
        let v = super::super::validate_json(
            &e,
            raw,
            &s,
            None,
            &ValidateOpts {
                images_max_n: 2,
                templated: false,
            },
        )
        .unwrap_err();
        assert_eq!((v.path.as_str(), v.kind), ("n", Val));
        let raw = br#"{"model":"m","prompt":"a cat","n":2}"#;
        let s = antirion_json::skim(raw, &antirion_json::JsonLimits::default()).unwrap();
        assert!(
            super::super::validate_json(
                &e,
                raw,
                &s,
                None,
                &ValidateOpts {
                    images_max_n: 2,
                    templated: false
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn speech() {
        rejects(SPEECH, r#"{"model":"m","voice":"alloy"}"#, "input", Some(Missing));
        rejects(SPEECH, r#"{"model":"m","input":"hello"}"#, "voice", Some(Missing));
        rejects(
            SPEECH,
            r#"{"model":"m","input":"hello","voice":5}"#,
            "voice",
            Some(Type),
        );
        assert_eq!(check_at(SPEECH, r#"{"input":"hello","voice":"alloy"}"#), Ok(()));
        assert_eq!(
            check_at(SPEECH, r#"{"model":"m","input":"hello","voice":{"id":"v1"}}"#),
            Ok(())
        );
    }

    #[test]
    fn completions_require_a_prompt() {
        rejects(
            "/v1/completions",
            r#"{"model":"m","messages":[{"role":"user","content":"hi"}]}"#,
            "prompt",
            Some(Missing),
        );
        rejects("/v1/completions", "{}", "prompt", Some(Missing));
        rejects("/v1/completions", r#"{"prompt":{}}"#, "prompt", Some(Type));
        rejects(
            "/v1/completions",
            r#"{"prompt":["a",1]}"#,
            "prompt[1]",
            Some(Type),
        );
        for ok in [r#""x""#, r#"["a","b"]"#, "[1,2]", "[[1],[2,3]]", "[]"] {
            assert_eq!(
                check_at("/v1/completions", &format!(r#"{{"prompt":{ok}}}"#)),
                Ok(()),
                "{ok}"
            );
        }
    }

    #[test]
    fn responses_need_something_to_answer() {
        rejects(
            RESP,
            r#"{"model":"m","instructions":"hi"}"#,
            "input",
            Some(Missing),
        );
        rejects(RESP, r#"{"model":"m","input":5}"#, "input", Some(Type));
        for ok in [
            r#"{"model":"m","input":"hi"}"#,
            r#"{"input":"hi"}"#,
            r#"{"model":"m","input":[{"type":"vendor_item","x":1},{"role":"user","content":"hi"}]}"#,
            r#"{"model":"m","previous_response_id":"resp_1"}"#,
            r#"{"model":"m","conversation":"conv_1"}"#,
        ] {
            assert_eq!(check_at(RESP, ok), Ok(()), "{ok}");
        }
    }

    #[test]
    fn anthropic_messages() {
        let body = r#"{"model":"m","max_tokens":8,"messages":[{"role":"user","content":"a"},{"role":"robot","content":"b"}]}"#;
        rejects(MSG, body, "messages[1].role", Some(Val));
        rejects(
            MSG,
            r#"{"model":"m","messages":[{"role":"user","content":"a"}]}"#,
            "max_tokens",
            Some(Missing),
        );
        rejects(
            MSG,
            r#"{"max_tokens":-1,"messages":[{"role":"user","content":"a"}]}"#,
            "max_tokens",
            Some(Val),
        );
        rejects(
            MSG,
            r#"{"max_tokens":1.5,"messages":[{"role":"user","content":"a"}]}"#,
            "max_tokens",
            Some(Type),
        );
        rejects(MSG, r#"{"max_tokens":1,"messages":[]}"#, "messages", Some(Val));
        rejects(MSG, r#"{"max_tokens":1,"messages":"x"}"#, "messages", Some(Type));
        rejects(
            MSG,
            r#"{"max_tokens":1,"messages":[{"role":"user","content":[{"text":"a"}]}]}"#,
            "messages[0].content[0].type",
            Some(Missing),
        );
        rejects(
            MSG,
            r#"{"max_tokens":1,"temperature":1.5,"messages":[{"role":"user","content":"a"}]}"#,
            "temperature",
            Some(Val),
        );
        rejects(
            MSG,
            r#"{"max_tokens":1,"thinking":{"type":"enabled","budget_tokens":1000},"messages":[{"role":"user","content":"a"}]}"#,
            "thinking.budget_tokens",
            Some(Val),
        );
        for ok in [
            r#"{"model":"m","max_tokens":0,"messages":[{"role":"user","content":"a"}]}"#,
            r#"{"max_tokens":8,"messages":[{"role":"user","content":"a"},{"role":"system","content":"s"}]}"#,
            r#"{"max_tokens":8,"thinking":{"type":"enabled","budget_tokens":1024},"messages":[{"role":"user","content":[{"type":"text","text":"a"}]}]}"#,
        ] {
            assert_eq!(check_at(MSG, ok), Ok(()), "{ok}");
        }
    }

    #[test]
    fn openai_shaped_messages_bodies_get_a_hint() {
        let v = check_at(
            MSG,
            r#"{"model":"m","max_completion_tokens":8,"messages":[{"role":"user","content":"a"}]}"#,
        )
        .unwrap_err();
        assert_eq!(v.path, "max_tokens");
        assert!(v.message.starts_with("max_tokens"));
        assert!(v.message.contains("/v1/chat/completions"));
        let v = check_at(
            MSG,
            r#"{"max_tokens":1,"messages":[{"role":"user","content":"a"},{"role":"x","content":"b"}]}"#,
        )
        .unwrap_err();
        assert!(!v.message.contains("/v1/chat/completions"));
    }
}

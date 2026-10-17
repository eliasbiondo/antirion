//! The prompt prefix of a request as a chain of chunk hashes: tool definitions, then each message
//! as `role\ntext\n`, cut into chunks of a fixed number of characters; `h_0` is the hash of the
//! pool, and each chunk's hash covers every chunk before it, so a shared prefix is a shared run of
//! leading hashes.

use serde_json::Value;

use crate::canon;

fn hash(prev: u64, chunk: &str) -> u64 {
    let mut buf = Vec::with_capacity(8 + chunk.len());
    buf.extend_from_slice(&prev.to_be_bytes());
    buf.extend_from_slice(chunk.as_bytes());
    xxhash_rust::xxh3::xxh3_64(&buf)
}

/// The serialised prompt of a request.
fn serialise(body: &Value) -> String {
    let mut out = String::new();
    if let Some(tools) = body.get("tools") {
        canon::write(tools, &mut out);
    }
    for (role, content) in [
        ("system", body.get("system")),
        ("system", body.get("instructions")),
        ("system", body.get("systemInstruction")),
    ] {
        if let Some(c) = content {
            out.push_str(role);
            out.push('\n');
            out.push_str(&canon::content_text(c.get("parts").unwrap_or(c)));
            out.push('\n');
        }
    }
    for m in canon::messages(body) {
        out.push_str(m.get("role").and_then(Value::as_str).unwrap_or_default());
        out.push('\n');
        out.push_str(&canon::content_text(canon::message_content(m)));
        out.push('\n');
    }
    if let Some(Value::String(s)) = body.get("input") {
        out.push_str("user\n");
        out.push_str(s);
        out.push('\n');
    }
    out
}

/// The chunk hashes of a request's prompt under the pool identified by `pool`.
#[must_use]
pub fn chain(pool: &str, body: &Value, chunk_chars: usize, max_chunks: usize) -> Vec<u64> {
    let text = serialise(body);
    let mut out = Vec::new();
    let mut prev = xxhash_rust::xxh3::xxh3_64(pool.as_bytes());
    let mut chunk = String::new();
    let mut n = 0;
    for c in text.chars() {
        chunk.push(c);
        n += 1;
        if n == chunk_chars.max(1) {
            prev = hash(prev, &chunk);
            out.push(prev);
            chunk.clear();
            n = 0;
            if out.len() >= max_chunks {
                return out;
            }
        }
    }
    if !chunk.is_empty() && out.len() < max_chunks {
        out.push(hash(prev, &chunk));
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_longer_conversation_extends_the_chain() {
        let system = "s".repeat(1000);
        let one =
            json!({"messages": [{"role": "system", "content": system}, {"role": "user", "content": "hi"}]});
        let two = json!({"messages": [{"role": "system", "content": system}, {"role": "user", "content": "hi"},
                                      {"role": "assistant", "content": "hello"}, {"role": "user", "content": "more"}]});
        let a = chain("p", &one, 256, 128);
        let b = chain("p", &two, 256, 128);
        assert_eq!(a.len(), 4);
        assert_eq!(a.get(..3), b.get(..3), "every full chunk of turn 1 leads turn 2");
        assert_ne!(chain("q", &one, 256, 128).first(), a.first());
        assert_eq!(chain("p", &two, 256, 2).len(), 2);
    }
}

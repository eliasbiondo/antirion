//! Where a client field sits in the client's body, the warning entries a translation produces, and
//! the refusals that make a deployment ineligible.

use serde_json::{Map, Value};

/// A client field: its depth-first position in the body (for ordering), its path with every index
/// replaced by `[]` (for warning entries) and its concrete path (for `param` and messages).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Src {
    order: Vec<u32>,
    path: String,
    at: String,
}

impl Src {
    /// The body itself.
    #[must_use]
    pub fn root() -> Src {
        Src::default()
    }

    /// A field the client did not send; it sorts after every field it sent.
    #[must_use]
    pub fn absent(name: &str) -> Src {
        Src {
            order: vec![u32::MAX],
            path: name.to_owned(),
            at: name.to_owned(),
        }
    }

    fn join(base: &str, name: &str) -> String {
        if base.is_empty() {
            name.to_owned()
        } else {
            format!("{base}.{name}")
        }
    }

    /// The member `name` of the object at this position.
    #[must_use]
    pub fn key(&self, obj: &Map<String, Value>, name: &str) -> Src {
        let pos = obj
            .keys()
            .position(|k| k == name)
            .map_or(u32::MAX, |p| u32::try_from(p).unwrap_or(u32::MAX));
        self.key_at(pos, name)
    }

    /// The member `name` at a known position.
    #[must_use]
    pub fn key_at(&self, pos: u32, name: &str) -> Src {
        let mut order = self.order.clone();
        order.push(pos);
        Src {
            order,
            path: Src::join(&self.path, name),
            at: Src::join(&self.at, name),
        }
    }

    /// The element `i` of the array at this position.
    #[must_use]
    pub fn idx(&self, i: usize) -> Src {
        let mut order = self.order.clone();
        order.push(u32::try_from(i).unwrap_or(u32::MAX));
        Src {
            order,
            path: format!("{}[]", self.path),
            at: format!("{}[{i}]", self.at),
        }
    }

    /// The path with indexes as `[]`.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The path with concrete indexes.
    #[must_use]
    pub fn at(&self) -> &str {
        &self.at
    }

    /// The depth-first position, for ordering.
    #[must_use]
    pub fn order(&self) -> &[u32] {
        &self.order
    }

    /// This field with the path of another (same position): for entries that name a different
    /// client field than the one that sits there.
    #[must_use]
    pub fn renamed(&self, path: &str) -> Src {
        Src {
            order: self.order.clone(),
            path: path.to_owned(),
            at: path.to_owned(),
        }
    }
}

/// The kind of a warning entry.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Ignored,
    Dropped,
    Approximated,
    Clamped,
    Defaulted,
    Emulated,
    Unknown,
}

impl Kind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Ignored => "ignored",
            Kind::Dropped => "dropped",
            Kind::Approximated => "approximated",
            Kind::Clamped => "clamped",
            Kind::Defaulted => "defaulted",
            Kind::Emulated => "emulated",
            Kind::Unknown => "unknown",
        }
    }

    /// Whether the kind makes a deployment ineligible under `on_lossy: reject` (classes A, L and K).
    #[must_use]
    pub fn strict(self) -> bool {
        !matches!(self, Kind::Ignored | Kind::Defaulted)
    }
}

/// One request-side entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warn {
    pub kind: Kind,
    pub src: Src,
}

impl Warn {
    /// The entry as the warnings header lists it: `kind=path`.
    #[must_use]
    pub fn entry(&self) -> String {
        format!("{}={}", self.kind.as_str(), self.src.path)
    }
}

/// The entries of one translation: request-side entries in client order, then response-side ones.
#[derive(Clone, Debug, Default)]
pub struct Warnings {
    req: Vec<Warn>,
    resp: Vec<String>,
}

const MAX_ENTRIES: usize = 32;
const MAX_BYTES: usize = 4096;

impl Warnings {
    pub fn add(&mut self, kind: Kind, src: &Src) {
        self.req.push(Warn {
            kind,
            src: src.clone(),
        });
    }

    /// A response-side entry, `path` naming the upstream element (without the `response:` prefix).
    pub fn response(&mut self, kind: Kind, path: &str) {
        let entry = format!("{}=response:{path}", kind.as_str());
        if !self.resp.contains(&entry) {
            self.resp.push(entry);
        }
    }

    /// Appends another translation's entries.
    pub fn extend(&mut self, other: &Warnings) {
        self.req.extend(other.req.iter().cloned());
        for e in &other.resp {
            if !self.resp.contains(e) {
                self.resp.push(e.clone());
            }
        }
    }

    /// The request-side entries in the order of a depth-first walk of the client body.
    fn sorted(&self) -> Vec<&Warn> {
        let mut req: Vec<&Warn> = self.req.iter().collect();
        req.sort_by(|a, b| a.src.order.cmp(&b.src.order));
        req
    }

    /// The first entry, in client order, that the `reject` policy refuses.
    #[must_use]
    pub fn first_strict(&self) -> Option<Warn> {
        self.sorted().into_iter().find(|w| w.kind.strict()).cloned()
    }

    /// Every entry, de-duplicated: request-side entries first, then response-side ones.
    #[must_use]
    pub fn entries(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for w in self.sorted() {
            let e = w.entry();
            if !out.contains(&e) {
                out.push(e);
            }
        }
        for e in &self.resp {
            if !out.contains(e) {
                out.push(e.clone());
            }
        }
        out
    }

    /// Only the request-side entries.
    #[must_use]
    pub fn request_entries(&self) -> Vec<String> {
        Warnings {
            req: self.req.clone(),
            resp: Vec::new(),
        }
        .entries()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.req.is_empty() && self.resp.is_empty()
    }
}

/// `x-antirion-translation-warnings` for a list of entries: at most 32 entries and 4096 bytes, the
/// rest counted in a final `truncated=<n>`.
#[must_use]
pub fn header_value(entries: &[String]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    const TAIL_ROOM: usize = 24;
    let mut out = String::new();
    let mut kept = 0usize;
    for (i, e) in entries.iter().enumerate() {
        let sep = if out.is_empty() { "" } else { ", " };
        let room = if i + 1 < entries.len() { TAIL_ROOM } else { 0 };
        if kept == MAX_ENTRIES || out.len() + sep.len() + e.len() + room > MAX_BYTES {
            break;
        }
        out.push_str(sep);
        out.push_str(e);
        kept += 1;
    }
    if kept < entries.len() {
        let sep = if out.is_empty() { "" } else { ", " };
        out.push_str(&format!("{sep}truncated={}", entries.len() - kept));
    }
    Some(out)
}

/// Why a deployment cannot take a request.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RefusalKind {
    /// A feature of class U for the target.
    Unsupported,
    /// A feature of class A, L or K under `on_lossy: reject`.
    Strict,
    /// A tool result that answers no earlier tool call: the request itself is invalid.
    Unpaired,
}

/// A deployment the request cannot be translated for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub kind: RefusalKind,
    /// The concrete path of the offending client field.
    pub param: String,
    pub message: String,
}

impl Refusal {
    #[must_use]
    pub fn unsupported(src: &Src, message: impl Into<String>) -> Refusal {
        Refusal {
            kind: RefusalKind::Unsupported,
            param: src.at().to_owned(),
            message: message.into(),
        }
    }

    /// The refusal of an inexact translation under the `reject` translation policy.
    #[must_use]
    pub fn strict(w: &Warn) -> Refusal {
        Refusal {
            kind: RefusalKind::Strict,
            param: w.src.at().to_owned(),
            message: format!(
                "`{}` cannot be translated exactly for this deployment ({}), and the translation policy is reject.",
                w.src.at(),
                w.kind.as_str()
            ),
        }
    }

    /// The refusal of a tool result whose id answers no earlier tool call.
    #[must_use]
    pub fn unpaired(src: &Src) -> Refusal {
        Refusal {
            kind: RefusalKind::Unpaired,
            param: src.at().to_owned(),
            message: format!(
                "`{}` does not match the id of a tool call in an earlier assistant turn.",
                src.at()
            ),
        }
    }

    /// The reason name of the refusal.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self.kind {
            RefusalKind::Unsupported => "translation_unsupported",
            RefusalKind::Strict => "translation_strict",
            RefusalKind::Unpaired => "tool_result_unpaired",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_follow_the_body_and_dedupe() {
        let body = serde_json::json!({"a": 1, "messages": [{"bar": 1}, {"bar": 2}], "z": 2});
        let obj = body.as_object().unwrap();
        let root = Src::root();
        let mut w = Warnings::default();
        w.add(Kind::Dropped, &root.key(obj, "z"));
        let msgs = root.key(obj, "messages");
        w.add(Kind::Unknown, &msgs.idx(1).key_at(0, "bar"));
        w.add(Kind::Unknown, &msgs.idx(0).key_at(0, "bar"));
        w.add(Kind::Defaulted, &Src::absent("max_tokens"));
        w.add(Kind::Ignored, &root.key(obj, "a"));
        w.response(Kind::Approximated, "stop_reason");
        assert_eq!(
            w.entries(),
            [
                "ignored=a",
                "unknown=messages[].bar",
                "dropped=z",
                "defaulted=max_tokens",
                "approximated=response:stop_reason"
            ]
        );
        assert_eq!(w.first_strict().unwrap().src.at(), "messages[0].bar");
    }

    #[test]
    fn header_is_capped() {
        let entries: Vec<String> = (0..40).map(|i| format!("unknown=foo_{i:02}")).collect();
        let h = header_value(&entries).unwrap();
        let parts: Vec<&str> = h.split(", ").collect();
        assert_eq!(parts.len(), 33);
        assert_eq!(parts[32], "truncated=8");
        assert_eq!(header_value(&[]), None);
    }
}

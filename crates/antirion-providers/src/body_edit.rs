//! Member-level edits of a JSON object body that keep every untouched member's bytes.

use std::cell::OnceCell;

use antirion_egress::Payload;
use antirion_json::{JsonLimits, Skim, skim};
use bytes::Bytes;
use serde_json::Value;

/// Edits of a JSON object body: members replaced, added or removed. With no edit the body is the
/// client's bytes, as they came; otherwise the members are re-joined, each kept member's bytes
/// unchanged. The body is read (joined into one slice and skimmed) only when an edit asks about it.
pub(crate) struct BodyEdit<'a> {
    body: &'a Payload,
    view: OnceCell<(Bytes, Option<Skim>)>,
    set: Vec<(&'static str, Vec<u8>)>,
    remove: Vec<&'static str>,
}

impl<'a> BodyEdit<'a> {
    pub(crate) fn new(body: &'a Payload) -> BodyEdit<'a> {
        BodyEdit {
            body,
            view: OnceCell::new(),
            set: Vec::new(),
            remove: Vec::new(),
        }
    }

    fn view(&self) -> (&Bytes, Option<&Skim>) {
        let (raw, skim) = self.view.get_or_init(|| {
            let raw = self.body.contiguous();
            let limits = JsonLimits {
                max_depth: usize::MAX,
            };
            let sk = skim(&raw, &limits).ok().filter(|s| s.top_is_object);
            (raw, sk)
        });
        (raw, skim.as_ref())
    }

    pub(crate) fn has(&self, name: &str) -> bool {
        self.view().1.is_some_and(|s| s.member(name).is_some())
    }

    pub(crate) fn value(&self, name: &str) -> Option<Value> {
        let (raw, skim) = self.view();
        let m = skim?.member(name)?;
        serde_json::from_slice(raw.get(m.value.clone())?).ok()
    }

    pub(crate) fn set(&mut self, name: &'static str, v: &Value) {
        let bytes = serde_json::to_vec(v).unwrap_or_default();
        let (raw, skim) = self.view();
        let unchanged = skim
            .and_then(|s| s.member(name))
            .and_then(|m| raw.get(m.value.clone()))
            == Some(bytes.as_slice());
        if !unchanged {
            self.set.push((name, bytes));
        }
    }

    pub(crate) fn remove(&mut self, name: &'static str) {
        if self.has(name) {
            self.remove.push(name);
        }
    }

    pub(crate) fn build(self) -> Payload {
        if self.set.is_empty() && self.remove.is_empty() {
            return self.body.clone();
        }
        let (raw, skim) = self.view();
        let Some(sk) = skim else {
            return self.body.clone();
        };
        let mut out: Vec<u8> = Vec::with_capacity(raw.len() + 64);
        out.push(b'{');
        let mut first = true;
        let mut sep = |out: &mut Vec<u8>| {
            if !first {
                out.push(b',');
            }
            first = false;
        };
        for m in &sk.members {
            if self.remove.contains(&m.name.as_str()) {
                continue;
            }
            sep(&mut out);
            match self.set.iter().find(|(n, _)| *n == m.name) {
                Some((_, v)) => {
                    out.extend_from_slice(raw.get(m.name_at..m.value.start).unwrap_or_default());
                    out.extend_from_slice(v);
                }
                None => out.extend_from_slice(raw.get(m.name_at..m.value.end).unwrap_or_default()),
            }
        }
        for (n, v) in &self.set {
            if sk.member(n).is_none() {
                sep(&mut out);
                out.extend_from_slice(&serde_json::to_vec(n).unwrap_or_default());
                out.push(b':');
                out.extend_from_slice(v);
            }
        }
        out.push(b'}');
        Payload::from(out)
    }
}

//! Client codecs, request side: a client body decoded into the IR. Decoding never depends on the
//! target; it records every field it recognises with its position and every field it does not.

mod anthropic;
mod chat;
mod converse;
mod gemini;
mod responses;

use serde_json::Value;

use super::envelope;
use super::ir::{
    Block, Cache, Client, Family, Inert, Location, Media, Num, Reasoning, Request, Unknown, Unsupported,
    WebSearch,
};
use super::media::{image_mime, parse_data_url};
use super::util::{Obj, b64_decode};
use super::warn::Src;

/// A part the gateway cannot use as sent: 400 `invalid_request`, reason `media_invalid`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invalid {
    /// The concrete path of the offending field.
    pub param: String,
    pub message: String,
}

/// Decodes a client request.
pub fn decode(client: Client, body: &Value) -> Result<Request, Invalid> {
    let Some(obj) = body.as_object() else {
        return Ok(Request::new(client));
    };
    let mut d = Dec {
        req: Request::new(client),
    };
    match client {
        Client::Chat => chat::decode(&mut d, obj)?,
        Client::Responses => responses::decode(&mut d, obj)?,
        Client::Anthropic => anthropic::decode(&mut d, obj)?,
        Client::Gemini => gemini::decode(&mut d, obj)?,
        Client::Converse => converse::decode(&mut d, obj)?,
    }
    d.req.unpaired = d.req.first_unpaired().cloned();
    Ok(d.req)
}

/// The decoding state.
pub(super) struct Dec {
    pub(super) req: Request,
}

pub(super) fn invalid(src: &Src, what: &str) -> Invalid {
    Invalid {
        param: src.at().to_owned(),
        message: format!("{} {what}", src.at()),
    }
}

impl Dec {
    /// Records the members of `obj` that are not in `known` as unknown fields.
    pub(super) fn unknown_members(&mut self, obj: &Obj, at: &Src, known: &[&str], top_level: bool) {
        for (k, v) in obj {
            if !known.contains(&k.as_str()) {
                self.req.unknown.push(Unknown {
                    name: k.clone(),
                    value: v.clone(),
                    top_level,
                    src: at.key(obj, k),
                });
            }
        }
    }

    pub(super) fn num(obj: &Obj, at: &Src, name: &str) -> Option<Num> {
        obj.get(name).filter(|v| !v.is_null()).map(|v| Num {
            value: v.clone(),
            src: at.key(obj, name),
        })
    }

    pub(super) fn inert(&mut self, obj: &Obj, at: &Src, name: &'static str) {
        if let Some(v) = obj.get(name).filter(|v| !v.is_null()) {
            self.req.inert.push(Inert {
                name,
                value: v.clone(),
                src: at.key(obj, name),
            });
        }
    }

    pub(super) fn unsupported(&mut self, src: Src, message: &str) {
        self.req.unsupported.push(Unsupported {
            src,
            message: message.to_owned(),
        });
    }
}

/// A `cache_control` member of `obj`.
pub(super) fn cache_of(obj: &Obj, at: &Src) -> Option<Cache> {
    obj.get("cache_control").filter(|v| v.is_object()).map(|v| Cache {
        value: v.clone(),
        src: at.key(obj, "cache_control"),
    })
}

/// An image URL (Chat `image_url.url`, Responses `image_url`): a data URL or a remote URL.
pub(super) fn image_media(url: Option<&Value>, url_src: &Src) -> Result<Media, Invalid> {
    let url = url.and_then(Value::as_str).filter(|u| !u.is_empty());
    let Some(url) = url else {
        return Err(invalid(url_src, "must be a non-empty URL."));
    };
    if url.starts_with("data:") {
        let d = parse_data_url(url).ok_or_else(|| invalid(url_src, "is not a valid base64 data URL."))?;
        let mime = image_mime(Some(&d.mime), &d.data);
        return Ok(Media::Base64 { mime, data: d.data });
    }
    Ok(Media::Url(url.to_owned()))
}

/// A document's data URL (Chat `file.file_data`, Responses `file_data`).
pub(super) fn document_media(data: &str, src: &Src) -> Result<Media, Invalid> {
    if !data.starts_with("data:") {
        return Ok(Media::Url(data.to_owned()));
    }
    let d = parse_data_url(data).ok_or_else(|| invalid(src, "is not a valid base64 data URL."))?;
    Ok(Media::Base64 {
        mime: d.mime,
        data: d.data,
    })
}

/// Checks that an inline base64 payload decodes.
pub(super) fn check_base64(data: &str, src: &Src) -> Result<(), Invalid> {
    b64_decode(data)
        .map(|_| ())
        .ok_or_else(|| invalid(src, "is not valid base64."))
}

/// A reasoning payload received from a client: an envelope is unwrapped; any other value belongs
/// to the client dialect's own family.
pub(super) fn reasoning(
    text: String,
    signature: Option<&str>,
    redacted: Option<&str>,
    native: Family,
    src: Src,
) -> Block {
    let mut family = native;
    let mut provider = None;
    let mut unwrap = |v: &str| -> String {
        match envelope::unwrap(v) {
            Some(u) => {
                family = u.family;
                provider = u.provider;
                u.payload
            }
            None if envelope::is_wrapped(v) => {
                family = Family::None;
                String::new()
            }
            None => v.to_owned(),
        }
    };
    let signature = signature.filter(|s| !s.is_empty()).map(&mut unwrap);
    let redacted = redacted.filter(|s| !s.is_empty()).map(&mut unwrap);
    Block::Reasoning(Reasoning {
        text,
        signature: signature.filter(|s| !s.is_empty()),
        redacted: redacted.filter(|s| !s.is_empty()),
        family,
        provider,
        restored: false,
        src,
    })
}

/// Chat `reasoning_details` entries (also accepted in the Chat extension): one reasoning block each.
pub(super) fn reasoning_details(list: &[Value], at: &Src) -> Vec<Block> {
    let mut out = Vec::new();
    for (i, d) in list.iter().enumerate() {
        let src = at.idx(i);
        let format = d
            .get("format")
            .and_then(Value::as_str)
            .map_or(Family::None, Family::parse);
        let text = d
            .get("text")
            .or_else(|| d.get("summary"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        match d.get("type").and_then(Value::as_str) {
            Some("reasoning.encrypted") => {
                let data = d.get("data").and_then(Value::as_str);
                let block = match format {
                    Family::Anthropic => reasoning(String::new(), None, data, format, src),
                    _ => reasoning(text, data, None, format, src),
                };
                out.push(block);
            }
            _ => out.push(reasoning(
                text,
                d.get("signature").and_then(Value::as_str),
                None,
                format,
                src,
            )),
        }
    }
    out
}

/// A user location: OpenAI's `{type: approximate, approximate: {...}}` or Anthropic's flat form.
fn location(v: Option<&Value>) -> Option<Location> {
    let v = v?;
    let fields = v.get("approximate").unwrap_or(v);
    let get = |k: &str| fields.get(k).and_then(Value::as_str).map(str::to_owned);
    let loc = Location {
        city: get("city"),
        region: get("region"),
        country: get("country"),
        timezone: get("timezone"),
    };
    (loc != Location::default()).then_some(loc)
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// Web search options of an OpenAI client (Chat `web_search_options`, a Responses `web_search`
/// tool) or an Anthropic `web_search_*` tool.
pub(super) fn web_search(v: &Value, src: Src) -> WebSearch {
    let mut w = WebSearch::at(src);
    w.max_uses = v.get("max_uses").and_then(Value::as_u64);
    w.allowed_domains = strings(
        v.get("allowed_domains")
            .or_else(|| v.pointer("/filters/allowed_domains")),
    );
    w.blocked_domains = strings(v.get("blocked_domains"));
    w.location = location(v.get("user_location"));
    w.context_size = v
        .get("search_context_size")
        .and_then(Value::as_str)
        .map(str::to_owned);
    w
}

//! Media for a target: which client URLs must be fetched, whether a format is accepted, and the
//! bytes or URL each block carries.

use std::collections::HashMap;

use super::Enc;
use crate::xlate::ir::{Block, Media, Request};
use crate::xlate::media::{admits, image_type_of_url, subtype};
use crate::xlate::target::{Target, Upstream};
use crate::xlate::warn::Src;

/// Bytes the gateway fetched for a client URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    pub mime: String,
    /// Base64.
    pub data: String,
}

/// Fetched media by URL.
pub type MediaMap = HashMap<String, Fetched>;

/// A URL the target needs as bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Need {
    pub url: String,
    pub image: bool,
    /// The concrete path of the field holding the URL.
    pub param: String,
}

/// Whether the target needs the bytes behind a URL.
fn must_fetch(t: &Target, image: bool, url: &str) -> bool {
    match (t.dialect, image) {
        (Upstream::Chat | Upstream::Responses, true) => false,
        (Upstream::Responses, false) => false,
        (Upstream::Chat, false) => true,
        (Upstream::Anthropic, true) => !t.image_url,
        (Upstream::Anthropic, false) => !t.document_url,
        (Upstream::Gemini, true) => !(t.image_url && image_type_of_url(url).is_some()),
        (Upstream::Gemini, false) => !t.document_url,
        (Upstream::Converse | Upstream::Cohere, _) => true,
    }
}

fn each_media<'a>(blocks: &'a [Block], out: &mut Vec<(&'a Media, bool, &'a Src)>) {
    for b in blocks {
        match b {
            Block::Image { media, url_src, .. } => out.push((media, true, url_src)),
            Block::Document { media, url_src, .. } => out.push((media, false, url_src)),
            Block::ToolResult(r) => each_media(&r.content, out),
            _ => {}
        }
    }
}

/// The client URLs `target` needs fetched, each once, in order.
#[must_use]
pub fn fetch_needs(req: &Request, target: &Target) -> Vec<Need> {
    let mut all = Vec::new();
    for t in &req.turns {
        each_media(&t.blocks, &mut all);
    }
    let mut out: Vec<Need> = Vec::new();
    for (media, image, src) in all {
        if let Media::Url(url) = media
            && must_fetch(target, image, url)
            && !out.iter().any(|n| &n.url == url)
        {
            out.push(Need {
                url: url.clone(),
                image,
                param: src.at().to_owned(),
            });
        }
    }
    out
}

/// What a target receives for a media block.
pub(super) enum Resolved {
    Bytes { mime: String, data: String },
    Url { url: String, mime: Option<String> },
    Text { mime: String, text: String },
}

impl Enc<'_> {
    /// The media a block sends, after the target's format and source checks; `None` when the block
    /// was refused.
    pub(super) fn resolve(
        &mut self,
        media: &Media,
        image: bool,
        src: &Src,
        url_src: &Src,
    ) -> Option<Resolved> {
        let t = self.t;
        let resolved = match media {
            Media::Base64 { mime, data } => Resolved::Bytes {
                mime: mime.clone(),
                data: data.clone(),
            },
            Media::Text { mime, text } => Resolved::Text {
                mime: mime.clone(),
                text: text.clone(),
            },
            Media::FileId(id) => {
                self.refuse(
                    src,
                    format!(
                        "The provider file id `{id}` at {} belongs to another provider.",
                        src.at()
                    ),
                );
                return None;
            }
            Media::Url(url) if must_fetch(t, image, url) => {
                if !t.media_fetch {
                    self.refuse(
                        url_src,
                        format!(
                            "The media at {} must be fetched, and media fetching is disabled.",
                            src.at()
                        ),
                    );
                    return None;
                }
                match self.media.and_then(|m| m.get(url)) {
                    Some(f) => Resolved::Bytes {
                        mime: f.mime.clone(),
                        data: f.data.clone(),
                    },
                    None if self.media.is_none() => Resolved::Bytes {
                        mime: if image { "image/png" } else { "application/pdf" }.to_owned(),
                        data: String::new(),
                    },
                    None => {
                        self.refuse(
                            url_src,
                            format!("The media at {} could not be fetched.", src.at()),
                        );
                        return None;
                    }
                }
            }
            Media::Url(url) => Resolved::Url {
                url: url.clone(),
                mime: if image {
                    image_type_of_url(url).map(str::to_owned)
                } else {
                    None
                },
            },
        };
        let mime = match &resolved {
            Resolved::Bytes { mime, .. } | Resolved::Text { mime, .. } => Some(mime.as_str()),
            Resolved::Url { mime, .. } => mime.as_deref(),
        };
        if let Some(mime) = mime {
            let ok = if image {
                admits(&t.image_formats, mime)
            } else {
                admits(&t.document_formats, mime)
                    || (t.dialect == Upstream::Anthropic && mime == "text/plain")
            };
            if !ok {
                let kind = if image { "image" } else { "document" };
                self.refuse(
                    src,
                    format!(
                        "The {kind} format `{}` at {} is not accepted by this deployment.",
                        subtype(mime),
                        src.at()
                    ),
                );
                return None;
            }
        }
        Some(resolved)
    }

    /// The audio part's format check: audio input reaches Gemini and Chat deployments only.
    pub(super) fn audio_ok(&mut self, format: &str, src: &Src) -> bool {
        if !matches!(self.t.dialect, Upstream::Gemini | Upstream::Chat) {
            self.refuse(
                src,
                format!(
                    "Audio input at {} cannot be sent to this deployment's API.",
                    src.at()
                ),
            );
            return false;
        }
        if !admits(&self.t.audio_formats, &format!("audio/{format}")) {
            self.refuse(
                src,
                format!(
                    "The audio format `{format}` at {} is not accepted by this deployment.",
                    src.at()
                ),
            );
            return false;
        }
        true
    }
}

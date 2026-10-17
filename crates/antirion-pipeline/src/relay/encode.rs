//! The client dialect a relayed stream is written in, and the frames of a translated stream.

use antirion_dialects::xlate::stream::Frame;
use bytes::Bytes;

use super::render::{chat_frame, converse_frame, gemini_frame, named};

/// The client dialect a relayed stream is written in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClientDialect {
    Chat,
    Responses,
    Anthropic,
    Gemini,
    Bedrock,
}

const DONE: &[u8] = b"data: [DONE]\n\n";

/// The bytes of one translated frame.
pub(crate) fn frame_bytes(f: &Frame) -> Bytes {
    match f {
        Frame::Data(v) => chat_frame(v),
        Frame::Named(v) => named(v),
        Frame::Done => Bytes::from_static(DONE),
        Frame::Gemini(v) => gemini_frame(v),
        Frame::Event(kind, v) => converse_frame(kind, v),
    }
}

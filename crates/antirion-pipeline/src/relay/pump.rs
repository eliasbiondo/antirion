//! The upstream side of a stream: the decoded body split into events, from server-sent events, AWS
//! event-stream messages, newline-delimited JSON or a streamed JSON array, under the stream's byte
//! cap.

use std::collections::VecDeque;
use std::pin::Pin;

use antirion_egress::{BodyError, InboundBody};
use antirion_wire::eventstream::{self, EventStreamError};
use antirion_wire::jsonstream::{JsonArrayParser, JsonEvent, JsonStreamError, NdjsonParser};
use antirion_wire::sse::{SseError, SseEvent, SseParser};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use http_body_util::BodyExt;

use super::events::StreamDialect;

/// Why an upstream byte stream ended.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum End {
    Clean,
    Abnormal,
    TooLarge,
    Malformed,
    /// The stream carried more than `max_response_bytes`.
    ResponseTooLarge,
}

/// How an upstream frames its stream.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Framing {
    Sse,
    EventStream,
    Ndjson,
    JsonArray,
}

/// The kind of an event-stream message; SSE events are always `Event`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    Event,
    /// An `exception` message, with its `:exception-type`.
    Exception(String),
    /// An `error` message, with its `:error-code`.
    Error(String),
}

/// One upstream event: its original bytes, its type and its data.
#[derive(Clone, Debug)]
pub(crate) struct UpEvent {
    pub(crate) raw: Bytes,
    pub(crate) event: Option<String>,
    pub(crate) data: String,
    pub(crate) frame: Frame,
}

impl From<SseEvent> for UpEvent {
    fn from(e: SseEvent) -> UpEvent {
        UpEvent {
            raw: e.raw,
            event: e.event,
            data: e.data,
            frame: Frame::Event,
        }
    }
}

impl From<JsonEvent> for UpEvent {
    fn from(e: JsonEvent) -> UpEvent {
        UpEvent {
            raw: e.raw,
            event: None,
            data: e.data,
            frame: Frame::Event,
        }
    }
}

fn from_message(m: eventstream::Message) -> UpEvent {
    let frame = match m.header(":message-type") {
        Some("exception") => Frame::Exception(m.header(":exception-type").unwrap_or_default().to_owned()),
        Some("error") => Frame::Error(m.header(":error-code").unwrap_or_default().to_owned()),
        _ => Frame::Event,
    };
    let mut data = String::from_utf8_lossy(&m.payload).into_owned();
    if let Frame::Error(_) = frame
        && data.trim().is_empty()
    {
        data = serde_json::json!({"message": m.header(":error-message").unwrap_or_default()}).to_string();
    }
    UpEvent {
        event: m.header(":event-type").map(str::to_owned),
        raw: m.raw,
        data,
        frame,
    }
}

pub(crate) enum Pulled {
    Event(UpEvent),
    /// A line completed without completing an event.
    Activity,
    End(End),
}

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, BodyError>> + Send>>;

enum Parser {
    Sse(SseParser),
    EventStream(eventstream::Decoder),
    Ndjson(NdjsonParser),
    JsonArray(JsonArrayParser),
}

/// The upstream body decoded and split into events.
pub(crate) struct Pump {
    src: Option<ByteStream>,
    parser: Parser,
    queue: VecDeque<UpEvent>,
    ended: Option<End>,
    /// Decoded bytes read so far, and the most the stream may carry.
    read: u64,
    cap: Option<u64>,
}

fn sse_end(e: SseError) -> End {
    match e {
        SseError::TooLarge => End::TooLarge,
        SseError::Malformed => End::Malformed,
    }
}

fn es_end(e: EventStreamError) -> End {
    match e {
        EventStreamError::TooLarge => End::TooLarge,
        EventStreamError::Malformed => End::Malformed,
    }
}

fn json_end(e: JsonStreamError) -> End {
    match e {
        JsonStreamError::TooLarge => End::TooLarge,
        JsonStreamError::Malformed => End::Malformed,
        JsonStreamError::Truncated => End::Abnormal,
    }
}

impl Pump {
    pub(crate) fn new(
        body: InboundBody,
        max_event: usize,
        dialect: StreamDialect,
        framing: Framing,
        cap: Option<u64>,
    ) -> Pump {
        Pump {
            src: Some(Box::pin(body.into_data_stream())),
            parser: match framing {
                Framing::EventStream => Parser::EventStream(eventstream::Decoder::new(max_event)),
                Framing::Ndjson => Parser::Ndjson(NdjsonParser::new(max_event)),
                Framing::JsonArray => Parser::JsonArray(JsonArrayParser::new(max_event)),
                Framing::Sse if dialect == StreamDialect::Gemini => {
                    Parser::Sse(SseParser::new(max_event).with_bare_errors())
                }
                Framing::Sse => Parser::Sse(SseParser::new(max_event)),
            },
            queue: VecDeque::new(),
            ended: None,
            read: 0,
            cap,
        }
    }

    /// A source that has already ended cleanly, for streams synthesized from a complete response.
    pub(crate) fn finished() -> Pump {
        Pump {
            src: None,
            parser: Parser::Sse(SseParser::new(1)),
            queue: VecDeque::new(),
            ended: Some(End::Clean),
            read: 0,
            cap: None,
        }
    }

    /// Closes the upstream: an unfinished body is never drained for reuse.
    pub(super) fn close(&mut self) {
        self.src = None;
    }

    fn feed(&mut self, chunk: &[u8]) -> (Vec<UpEvent>, Option<End>) {
        match &mut self.parser {
            Parser::Sse(p) => {
                let mut out = Vec::new();
                let end = p.push(chunk, &mut out).err().map(sse_end);
                (out.into_iter().map(UpEvent::from).collect(), end)
            }
            Parser::EventStream(d) => {
                let mut out = Vec::new();
                let end = d.push(chunk, &mut out).err().map(es_end);
                (out.into_iter().map(from_message).collect(), end)
            }
            Parser::Ndjson(p) => {
                let mut out = Vec::new();
                let end = p.push(chunk, &mut out).err().map(json_end);
                (out.into_iter().map(UpEvent::from).collect(), end)
            }
            Parser::JsonArray(p) => {
                let mut out = Vec::new();
                let end = p.push(chunk, &mut out).err().map(json_end);
                (out.into_iter().map(UpEvent::from).collect(), end)
            }
        }
    }

    fn finish(&mut self) -> (Vec<UpEvent>, End) {
        match &mut self.parser {
            Parser::Sse(p) => match p.finish() {
                Ok(tail) => (tail.into_iter().map(UpEvent::from).collect(), End::Clean),
                Err(e) => (Vec::new(), sse_end(e)),
            },
            Parser::EventStream(d) if d.truncated() => (Vec::new(), End::Abnormal),
            Parser::EventStream(_) => (Vec::new(), End::Clean),
            Parser::Ndjson(p) => match p.finish() {
                Ok(tail) => (tail.into_iter().map(UpEvent::from).collect(), End::Clean),
                Err(e) => (Vec::new(), json_end(e)),
            },
            Parser::JsonArray(p) => match p.finish() {
                Ok(()) => (Vec::new(), End::Clean),
                Err(e) => (Vec::new(), json_end(e)),
            },
        }
    }

    fn activity(&self) -> u64 {
        match &self.parser {
            Parser::Sse(p) => p.lines(),
            Parser::Ndjson(p) => p.lines(),
            Parser::EventStream(_) | Parser::JsonArray(_) => 0,
        }
    }

    /// The next event, or line activity, or the end. Cancel-safe: state lives in `self`.
    pub(crate) async fn pull(&mut self) -> Pulled {
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return Pulled::Event(ev);
            }
            if let Some(end) = self.ended {
                return Pulled::End(end);
            }
            let lines = self.activity();
            let Some(src) = self.src.as_mut() else {
                self.ended = Some(End::Abnormal);
                continue;
            };
            match src.next().await {
                Some(Ok(chunk)) => {
                    self.read = self
                        .read
                        .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                    if self.cap.is_some_and(|cap| self.read > cap) {
                        self.ended = Some(End::ResponseTooLarge);
                        continue;
                    }
                    let (out, end) = self.feed(&chunk);
                    self.queue.extend(out);
                    self.ended = end;
                }
                Some(Err(_)) => self.ended = Some(End::Abnormal),
                None => {
                    let (out, end) = self.finish();
                    self.queue.extend(out);
                    self.ended = Some(end);
                }
            }
            if self.queue.is_empty() && self.ended.is_none() && self.activity() != lines {
                return Pulled::Activity;
            }
        }
    }
}

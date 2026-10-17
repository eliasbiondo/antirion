//! An incremental Server-Sent Events parser over a byte stream.

use std::time::Duration;

use antirion_egress::InboundBody;
use bytes::BytesMut;
use http_body_util::BodyExt;

/// One parsed event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Event {
    pub(crate) event: Option<String>,
    pub(crate) data: String,
    pub(crate) id: Option<String>,
}

/// Splits bytes into events as they arrive. Comment lines are skipped; an event without data is not
/// reported.
#[derive(Debug, Default)]
pub(crate) struct Parser {
    buf: BytesMut,
    current: Event,
    has_data: bool,
}

impl Parser {
    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// The next complete event, if the buffered bytes hold one.
    pub(crate) fn next_event(&mut self) -> Option<Event> {
        loop {
            let end = self.buf.iter().position(|&b| b == b'\n')?;
            let mut line = self.buf.split_to(end + 1);
            line.truncate(end);
            if line.last() == Some(&b'\r') {
                line.truncate(end.saturating_sub(1));
            }
            if line.is_empty() {
                if self.has_data {
                    self.has_data = false;
                    return Some(std::mem::take(&mut self.current));
                }
                self.current = Event::default();
                continue;
            }
            if line.first() == Some(&b':') {
                continue;
            }
            let text = String::from_utf8_lossy(&line).into_owned();
            let (field, value) = match text.split_once(':') {
                Some((f, v)) => (f.to_owned(), v.strip_prefix(' ').unwrap_or(v).to_owned()),
                None => (text.clone(), String::new()),
            };
            match field.as_str() {
                "event" => self.current.event = Some(value),
                "id" => self.current.id = Some(value),
                "data" => {
                    if self.has_data {
                        self.current.data.push('\n');
                    }
                    self.current.data.push_str(&value);
                    self.has_data = true;
                }
                _ => {}
            }
        }
    }

    #[cfg(test)]
    fn remaining(&self) -> usize {
        self.buf.len()
    }
}

/// Why reading the next event failed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReadError {
    /// Nothing arrived within the idle bound.
    Idle,
    /// The stream ended or broke.
    Ended,
}

/// Events read from an upstream response body, with a bound on silence between them.
#[derive(Debug)]
pub(crate) struct Reader {
    body: InboundBody,
    parser: Parser,
}

impl Reader {
    pub(crate) fn new(body: InboundBody) -> Reader {
        Reader {
            body,
            parser: Parser::default(),
        }
    }

    pub(crate) async fn next(&mut self, idle: Duration) -> Result<Event, ReadError> {
        loop {
            if let Some(ev) = self.parser.next_event() {
                return Ok(ev);
            }
            match tokio::time::timeout(idle, self.body.frame()).await {
                Err(_) => return Err(ReadError::Idle),
                Ok(None | Some(Err(_))) => return Err(ReadError::Ended),
                Ok(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        self.parser.push(&data);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_across_chunks_with_comments() {
        let mut p = Parser::default();
        p.push(b": ping\n\n: keepalive\n\nevent: other\ndata: {}\n\nevent: endpoint\nda");
        assert_eq!(
            p.next_event(),
            Some(Event {
                event: Some("other".into()),
                data: "{}".into(),
                id: None
            })
        );
        assert_eq!(p.next_event(), None);
        p.push(b"ta: /x/messages?sessionId=1\r\n\r\nid: 7\ndata: a\ndata: b\n\n");
        let e = p.next_event().unwrap();
        assert_eq!(e.event.as_deref(), Some("endpoint"));
        assert_eq!(e.data, "/x/messages?sessionId=1");
        let e = p.next_event().unwrap();
        assert_eq!((e.id.as_deref(), e.data.as_str()), (Some("7"), "a\nb"));
        assert_eq!(p.remaining(), 0);
    }
}

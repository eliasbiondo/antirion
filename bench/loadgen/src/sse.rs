//! Event timing of a server-sent-event stream, read as it arrives: when the first event that carries
//! content came, and the gap before every later event.

use std::time::Instant;

/// Follows one stream's events across body frames.
#[derive(Debug, Default)]
pub struct EventClock {
    partial: Vec<u8>,
    last: Option<Instant>,
    first_content: Option<Instant>,
    events: u64,
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// An event carries content when it is a Messages text delta, or a Chat chunk whose `content` string
/// is not empty (the role-only chunk carries `"content":""`).
fn carries_content(event: &[u8]) -> bool {
    if find(event, b"content_block_delta").is_some() {
        return true;
    }
    let mut rest = event;
    while let Some(i) = find(rest, br#""content":""#) {
        let after = rest.get(i + 11..).unwrap_or_default();
        if after.first().is_some_and(|&c| c != b'"') {
            return true;
        }
        rest = after;
    }
    false
}

impl EventClock {
    /// Feeds one body frame that arrived at `now`; returns the gaps it closed.
    pub fn feed(&mut self, frame: &[u8], now: Instant, gaps: &mut Vec<std::time::Duration>) {
        self.partial.extend_from_slice(frame);
        while let Some(end) = find(&self.partial, b"\n\n") {
            let event: Vec<u8> = self.partial.drain(..end + 2).collect();
            if find(&event, b"data:").is_none() {
                continue;
            }
            self.events += 1;
            if let Some(prev) = self.last {
                gaps.push(now.saturating_duration_since(prev));
            }
            self.last = Some(now);
            if self.first_content.is_none() && carries_content(&event) {
                self.first_content = Some(now);
            }
        }
    }

    /// When the first content event arrived.
    #[must_use]
    pub fn first_content(&self) -> Option<Instant> {
        self.first_content
    }

    #[must_use]
    pub fn events(&self) -> u64 {
        self.events
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn role_chunk_is_not_content() {
        assert!(!carries_content(
            br#"data: {"choices":[{"delta":{"role":"assistant","content":""}}]}"#
        ));
        assert!(carries_content(
            br#"data: {"choices":[{"delta":{"content":"hi"}}]}"#
        ));
        assert!(carries_content(b"event: content_block_delta\ndata: {}"));
        assert!(!carries_content(b"event: message_start\ndata: {}"));
    }

    #[test]
    fn events_split_across_frames() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(5);
        let mut c = EventClock::default();
        let mut gaps = Vec::new();
        c.feed(
            br#"data: {"delta":{"role":"assistant","content":""}}"#,
            t0,
            &mut gaps,
        );
        c.feed(b"\n\ndata: {\"delta\":{\"content\":\"a", t0, &mut gaps);
        assert_eq!(c.events(), 1);
        assert!(c.first_content().is_none());
        c.feed(b"\"}}\n\ndata: [DONE]\n\n", t1, &mut gaps);
        assert_eq!(c.events(), 3);
        assert_eq!(c.first_content(), Some(t1));
        assert_eq!(gaps, vec![Duration::from_millis(5), Duration::ZERO]);
    }
}

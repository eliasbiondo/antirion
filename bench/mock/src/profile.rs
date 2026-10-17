//! Latency profiles: when the mock sends headers and each streamed event.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

/// A latency profile of the bench mock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Profile {
    /// Respond immediately; streams send every event at once.
    L0,
    /// 60 ms before headers; streams: first event after 60 ms, then one every 5 ms.
    L60,
    /// 1,500 ms before headers.
    L1500,
    /// Streams: first event after 200 ms, then one every 20 ms (a 1 s stream).
    Ls,
    /// `Ls` stretched to 10 s: first event after 200 ms, then one every 245 ms.
    Ls10s,
    /// Streams: headers and the first event, then silence until the client leaves.
    LsIdle,
}

/// Content events of every stream after the first event.
pub const STREAM_EVENTS: u32 = 40;

impl Profile {
    pub const ALL: [Profile; 6] = [
        Profile::L0,
        Profile::L60,
        Profile::L1500,
        Profile::Ls,
        Profile::Ls10s,
        Profile::LsIdle,
    ];

    /// The wait before a complete (non-streaming) response.
    #[must_use]
    pub fn head_delay(self) -> Duration {
        match self {
            Profile::L60 => Duration::from_millis(60),
            Profile::L1500 => Duration::from_millis(1500),
            Profile::Ls | Profile::Ls10s => Duration::from_millis(200),
            Profile::L0 | Profile::LsIdle => Duration::ZERO,
        }
    }

    /// The wait before a stream's first event.
    #[must_use]
    pub fn first_event(self) -> Duration {
        self.head_delay()
    }

    /// The wait between later events.
    #[must_use]
    pub fn spacing(self) -> Duration {
        match self {
            Profile::L60 => Duration::from_millis(5),
            Profile::Ls => Duration::from_millis(20),
            Profile::Ls10s => Duration::from_millis(245),
            Profile::L0 | Profile::L1500 | Profile::LsIdle => Duration::ZERO,
        }
    }

    /// Whether a stream stops after its first event and never ends.
    #[must_use]
    pub fn idles(self) -> bool {
        self == Profile::LsIdle
    }

    /// How long the mock itself takes to answer: the scripted delay a direct run cannot beat.
    #[must_use]
    pub fn scripted(self, stream: bool) -> Duration {
        if stream {
            self.first_event() + self.spacing() * STREAM_EVENTS
        } else {
            self.head_delay()
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Profile::L0 => "L0",
            Profile::L60 => "L60",
            Profile::L1500 => "L1500",
            Profile::Ls => "LS",
            Profile::Ls10s => "LS-10s",
            Profile::LsIdle => "LS-idle",
        }
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown latency profile {0:?} (L0, L60, L1500, LS, LS-10s, LS-idle)")]
pub struct UnknownProfile(pub String);

impl FromStr for Profile {
    type Err = UnknownProfile;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Profile::ALL
            .into_iter()
            .find(|p| p.name().eq_ignore_ascii_case(s))
            .ok_or_else(|| UnknownProfile(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for p in Profile::ALL {
            assert_eq!(p.name().parse::<Profile>().unwrap(), p);
        }
        assert!("L5".parse::<Profile>().is_err());
    }

    #[test]
    fn stream_lengths_match_the_matrix() {
        assert_eq!(Profile::Ls.scripted(true), Duration::from_secs(1));
        assert_eq!(Profile::Ls10s.scripted(true), Duration::from_millis(10_000));
        assert_eq!(Profile::L60.scripted(false), Duration::from_millis(60));
        assert_eq!(Profile::L0.scripted(true), Duration::ZERO);
    }
}

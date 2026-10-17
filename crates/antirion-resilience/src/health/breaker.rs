//! A deployment's circuit breaker: consecutive failures and a bucketed failure-rate window trip it
//! open; it half-opens when the open period ends and closes after enough successful probes. Each
//! trip doubles the open period, up to its maximum.

use std::collections::VecDeque;
use std::time::Duration;

use antirion_core::UtcMicros;

use crate::class::BreakerEffect;
use crate::settings::BreakerSettings;

/// Breaker states.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

impl BreakerState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            BreakerState::Closed => "closed",
            BreakerState::Open => "open",
            BreakerState::HalfOpen => "half_open",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Bucket {
    index: i64,
    requests: u32,
    failures: u32,
}

#[derive(Clone, Debug)]
pub(super) struct Breaker {
    pub(super) state: BreakerState,
    pub(super) consecutive: u32,
    successes: u32,
    window: VecDeque<Bucket>,
    pub(super) trips: u32,
    pub(super) open_until: Option<UtcMicros>,
    pub(super) probes: u32,
    closed_since: UtcMicros,
    /// How many times a success reset the consecutive count.
    pub(super) resets: u64,
    /// The window's failure rate when the breaker last tripped.
    pub(super) trip_rate: Option<f64>,
}

/// What a success recorded at a stream's commit changed, so a failure after the commit can take
/// its place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SuccessMark {
    /// The consecutive count the success reset.
    before: u32,
    /// `resets` after the success: no other success came since while it is unchanged.
    epoch: u64,
    bucket: i64,
}

impl Default for Breaker {
    fn default() -> Breaker {
        Breaker {
            state: BreakerState::Closed,
            consecutive: 0,
            successes: 0,
            window: VecDeque::new(),
            trips: 0,
            open_until: None,
            probes: 0,
            closed_since: UtcMicros::EPOCH,
            resets: 0,
            trip_rate: None,
        }
    }
}

pub(super) fn after(now: UtcMicros, d: Duration) -> UtcMicros {
    now.saturating_add(d)
}

pub(super) fn doubled(base: Duration, k: u32, max: Duration) -> Duration {
    let factor = 2u32.checked_pow(k.saturating_sub(1)).unwrap_or(u32::MAX);
    base.checked_mul(factor).unwrap_or(max).min(max)
}

fn bucket_width(s: &BreakerSettings) -> i64 {
    let micros = s.window.as_micros() / u128::from(s.buckets.max(1));
    i64::try_from(micros).unwrap_or(i64::MAX).max(1)
}

impl Breaker {
    /// Applies the passage of time: an open breaker half-opens, trips decay after a long close.
    pub(super) fn refresh(&mut self, s: &BreakerSettings, now: UtcMicros) {
        if self.state == BreakerState::Open && self.open_until.is_none_or(|u| now >= u) {
            self.state = BreakerState::HalfOpen;
            self.probes = 0;
            self.successes = 0;
        }
        if self.state == BreakerState::Closed
            && self.trips > 0
            && now.since(self.closed_since) >= s.max_open_duration
        {
            self.trips = 0;
        }
    }

    fn prune(&mut self, s: &BreakerSettings, now: UtcMicros) -> i64 {
        let index = now.0.div_euclid(bucket_width(s));
        let oldest = index - i64::from(s.buckets.max(1)) + 1;
        while self.window.front().is_some_and(|b| b.index < oldest) {
            self.window.pop_front();
        }
        index
    }

    pub(super) fn totals(&mut self, s: &BreakerSettings, now: UtcMicros) -> (u32, u32) {
        self.prune(s, now);
        self.window.iter().fold((0, 0), |(r, f), b| {
            (r.saturating_add(b.requests), f.saturating_add(b.failures))
        })
    }

    fn record(&mut self, s: &BreakerSettings, now: UtcMicros, failure: bool) {
        let index = self.prune(s, now);
        match self.window.back_mut() {
            Some(b) if b.index == index => {
                b.requests += 1;
                b.failures += u32::from(failure);
            }
            _ => self.window.push_back(Bucket {
                index,
                requests: 1,
                failures: u32::from(failure),
            }),
        }
    }

    pub(super) fn trip(&mut self, s: &BreakerSettings, now: UtcMicros) {
        let (requests, failures) = self.totals(s, now);
        self.trip_rate = (requests > 0).then(|| f64::from(failures) / f64::from(requests));
        self.trips = self.trips.saturating_add(1);
        self.state = BreakerState::Open;
        self.probes = 0;
        self.successes = 0;
        self.open_until = Some(after(
            now,
            doubled(s.open_duration, self.trips, s.max_open_duration),
        ));
    }

    fn trip_if_due(&mut self, s: &BreakerSettings, now: UtcMicros) {
        let by_count = s.consecutive_failures.is_some_and(|n| self.consecutive >= n);
        let (requests, failures) = self.totals(s, now);
        let by_rate = requests >= s.min_requests
            && requests > 0
            && f64::from(failures) / f64::from(requests) >= s.failure_rate;
        if by_count || by_rate {
            self.trip(s, now);
        }
    }

    /// The mark of a success about to be recorded in the closed state.
    pub(super) fn mark(&self, s: &BreakerSettings, now: UtcMicros) -> SuccessMark {
        SuccessMark {
            before: self.consecutive,
            epoch: self.resets.wrapping_add(1),
            bucket: now.0.div_euclid(bucket_width(s)),
        }
    }

    /// A stream whose success was recorded at its commit failed afterwards: the failure takes the
    /// success's place. The consecutive count it reset is restored when no other success came
    /// since, and its window entry becomes a failure while it is in the window.
    pub(super) fn failed_after_success(&mut self, s: &BreakerSettings, now: UtcMicros, m: SuccessMark) {
        if !s.enabled {
            return;
        }
        match self.state {
            BreakerState::Closed => {
                let restored = if self.resets == m.epoch { m.before } else { 0 };
                self.consecutive = self.consecutive.saturating_add(restored).saturating_add(1);
                self.prune(s, now);
                if let Some(b) = self.window.iter_mut().find(|b| b.index == m.bucket) {
                    b.failures = b.failures.saturating_add(1).min(b.requests);
                }
                self.trip_if_due(s, now);
            }
            BreakerState::HalfOpen => self.trip(s, now),
            BreakerState::Open => {}
        }
    }

    /// The verdict of an active check: a success counts as a probe success in the open and
    /// half-open states, a failure re-opens a half-open breaker; in the closed state both count as
    /// a request's verdict would.
    pub(super) fn checked(&mut self, s: &BreakerSettings, now: UtcMicros, ok: bool) {
        if !s.enabled {
            return;
        }
        match (self.state, ok) {
            (BreakerState::Closed, true) => self.verdict(s, now, BreakerEffect::Ok, false),
            (BreakerState::Closed, false) => self.verdict(s, now, BreakerEffect::Fail, false),
            (BreakerState::Open | BreakerState::HalfOpen, true) => {
                self.successes = self.successes.saturating_add(1);
                if self.successes >= s.success_threshold {
                    self.close(now);
                }
            }
            (BreakerState::Open, false) => self.successes = 0,
            (BreakerState::HalfOpen, false) => self.trip(s, now),
        }
    }

    /// Takes a state another node published: an open period, or a close.
    pub(super) fn adopt(&mut self, open: Option<UtcMicros>, trips: u32, now: UtcMicros) {
        self.trips = trips;
        match open {
            Some(until) => {
                self.state = BreakerState::Open;
                self.open_until = Some(until);
                self.successes = 0;
            }
            None if self.state != BreakerState::Closed => {
                let probes = self.probes;
                self.close(now);
                self.probes = probes;
            }
            None => {}
        }
    }

    pub(super) fn close(&mut self, now: UtcMicros) {
        self.state = BreakerState::Closed;
        self.consecutive = 0;
        self.successes = 0;
        self.probes = 0;
        self.window.clear();
        self.open_until = None;
        self.closed_since = now;
    }

    pub(super) fn verdict(
        &mut self,
        s: &BreakerSettings,
        now: UtcMicros,
        effect: BreakerEffect,
        probe: bool,
    ) {
        if probe {
            self.probes = self.probes.saturating_sub(1);
        }
        if !s.enabled {
            return;
        }
        match (self.state, effect) {
            (_, BreakerEffect::Neutral) => {}
            (BreakerState::Closed, BreakerEffect::Ok) => {
                self.consecutive = 0;
                self.resets = self.resets.wrapping_add(1);
                self.record(s, now, false);
            }
            (BreakerState::Closed, BreakerEffect::Fail) => {
                self.consecutive = self.consecutive.saturating_add(1);
                self.record(s, now, true);
                self.trip_if_due(s, now);
            }
            (BreakerState::HalfOpen, BreakerEffect::Ok) if probe => {
                self.successes = self.successes.saturating_add(1);
                if self.successes >= s.success_threshold {
                    self.close(now);
                }
            }
            (BreakerState::HalfOpen, BreakerEffect::Fail) if probe => self.trip(s, now),
            _ => {}
        }
    }
}

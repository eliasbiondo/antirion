use std::collections::HashMap;
use std::sync::Arc;

use antirion_core::SystemClock;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use super::super::{KeyPick, LoopCx, Outcome, Selected, run};
use super::*;
use crate::class::RequestKind;
use crate::health::{HealthRegistry, KeyMode};
use crate::policy::Policy;
use crate::settings::HealthConfig;

/// Tiers of deployments, each tried in order.
struct Tiers {
    tiers: Vec<Vec<&'static str>>,
    tier: usize,
    pos: usize,
    policy: Arc<Policy>,
}

impl Planner for Tiers {
    fn select(&mut self, tried: &[String]) -> Option<Selected> {
        let tier = self.tiers.get(self.tier)?;
        while let Some(d) = tier.get(self.pos) {
            self.pos += 1;
            if !tried.iter().any(|t| t == d) {
                return Some(Selected {
                    deployment: (*d).to_owned(),
                    provider: (*d).to_owned(),
                    via: None,
                    policy: Arc::clone(&self.policy),
                });
            }
        }
        None
    }

    fn next_tier(&mut self) -> bool {
        self.tier += 1;
        self.pos = 0;
        self.tier < self.tiers.len()
    }

    fn attempted(&mut self, _s: &Selected) {}

    fn select_key(
        &mut self,
        _s: &Selected,
        _tried: &[String],
        _last: Option<&str>,
        _mode: KeyMode,
    ) -> KeyPick {
        KeyPick::Keyless
    }

    fn skip(&mut self, _s: &Selected, _reason: &'static str) {}

    async fn gate(&mut self, _s: &Selected) -> Result<(), &'static str> {
        Ok(())
    }
}

/// Answers each deployment after its delay with its class; records starts and cancellations.
#[derive(Default)]
struct Timed {
    answers: HashMap<&'static str, (u64, Class)>,
    started: Mutex<Vec<(String, bool)>>,
    cancelled: Arc<Mutex<Vec<String>>>,
}

struct Running {
    deployment: String,
    done: bool,
    cancelled: Arc<Mutex<Vec<String>>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        if !self.done {
            self.cancelled.lock().push(self.deployment.clone());
        }
    }
}

impl Sender for Timed {
    type Done = String;

    async fn send(&self, a: Attempt<'_>) -> Sent<String> {
        let d = a.selected.deployment.clone();
        self.started.lock().push((d.clone(), a.hedge));
        let (ms, class) = self
            .answers
            .get(d.as_str())
            .copied()
            .unwrap_or((0, Class::Success));
        let mut running = Running {
            deployment: d.clone(),
            done: false,
            cancelled: Arc::clone(&self.cancelled),
        };
        tokio::time::sleep(Duration::from_millis(ms)).await;
        running.done = true;
        if class == Class::Success {
            return Sent::Success(d);
        }
        Sent::Failed(Failure {
            class,
            status: None,
            timer: None,
            hint: None,
            error: Box::new(GatewayError::new(
                class.final_code(),
                Stage::Attempt,
                Message::Fixed("upstream"),
            )),
        })
    }
}

fn hedged(delay_ms: u64, max_extra: u32) -> Policy {
    let mut p = Policy::default();
    p.retry.backoff.initial = Duration::from_millis(1);
    p.retry.backoff.max = Duration::from_millis(2);
    p.hedging = Some(Hedging {
        delay: Duration::from_millis(delay_ms),
        max_extra,
    });
    p
}

async fn drive(
    tiers: Vec<Vec<&'static str>>,
    policy: Policy,
    up: &mut Timed,
    max_attempts: u32,
) -> Outcome<String> {
    let mut plan = Tiers {
        tiers,
        tier: 0,
        pos: 0,
        policy: Arc::new(policy),
    };
    let health = HealthRegistry::default();
    let cfg = HealthConfig::default();
    let cancel = CancellationToken::new();
    run(
        &mut plan,
        up,
        LoopCx {
            health: &health,
            health_cfg: &cfg,
            clock: &SystemClock,
            cancel: &cancel,
            kind: RequestKind::Generation,
            max_attempts,
            min_attempt_time: Duration::from_millis(1),
            deadline: Some(Duration::from_secs(30)),
            fallback: true,
            seed: 7,
            read_timeout: None,
            ignore_health: &[],
        },
    )
    .await
}

fn timed(answers: &[(&'static str, u64, Class)]) -> Timed {
    Timed {
        answers: answers.iter().map(|(d, ms, c)| (*d, (*ms, *c))).collect(),
        ..Timed::default()
    }
}

#[tokio::test]
async fn a_fast_hedge_wins_and_the_slow_first_attempt_is_cancelled() {
    let mut up = timed(&[("a", 2_000, Class::Success), ("b", 20, Class::Success)]);
    let started = Instant::now();
    let out = drive(vec![vec!["a", "b"]], hedged(100, 1), &mut up, 5).await;
    assert!(matches!(out, Outcome::Success(ref d) if d == "b"));
    assert!(
        started.elapsed() < Duration::from_millis(1_000),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        *up.started.lock(),
        vec![("a".to_owned(), false), ("b".to_owned(), true)]
    );
    assert_eq!(*up.cancelled.lock(), vec!["a".to_owned()]);
}

#[tokio::test]
async fn an_answer_before_the_delay_starts_no_hedge() {
    let mut up = timed(&[("a", 10, Class::Success)]);
    let out = drive(vec![vec!["a", "b"]], hedged(200, 1), &mut up, 5).await;
    assert!(matches!(out, Outcome::Success(ref d) if d == "a"));
    assert_eq!(up.started.lock().len(), 1);
}

#[tokio::test]
async fn a_failure_before_the_delay_goes_on_as_without_hedging() {
    let mut up = timed(&[("a", 5, Class::ServerError), ("b", 5, Class::Success)]);
    let out = drive(vec![vec!["a", "b"]], hedged(300, 1), &mut up, 5).await;
    assert!(matches!(out, Outcome::Success(ref d) if d == "b"));
    assert_eq!(
        *up.started.lock(),
        vec![("a".to_owned(), false), ("b".to_owned(), false)]
    );
}

#[tokio::test]
async fn when_every_raced_attempt_fails_the_loop_goes_on_from_the_last() {
    let mut up = timed(&[
        ("a", 300, Class::ServerError),
        ("b", 10, Class::ServerError),
        ("c", 10, Class::Success),
    ]);
    let out = drive(vec![vec!["a", "b"], vec!["c"]], hedged(50, 1), &mut up, 5).await;
    assert!(matches!(out, Outcome::Success(ref d) if d == "c"));
    let started: Vec<String> = up.started.lock().iter().map(|(d, _)| d.clone()).collect();
    assert_eq!(
        started,
        vec!["a", "b", "a", "c"],
        "the hedge was the tier's next deployment: one retry of the last failure is left"
    );
    assert!(up.cancelled.lock().is_empty());
}

#[tokio::test]
async fn hedges_are_capped_by_max_extra_and_max_attempts() {
    let slow = |d| (d, 400, Class::Success);
    let mut up = timed(&[slow("a"), slow("b"), slow("c"), slow("d")]);
    drive(vec![vec!["a", "b", "c", "d"]], hedged(30, 2), &mut up, 5).await;
    assert_eq!(up.started.lock().len(), 3, "the first attempt and two hedges");
    let mut up = timed(&[slow("a"), slow("b"), slow("c")]);
    drive(vec![vec!["a", "b", "c"]], hedged(30, 2), &mut up, 2).await;
    assert_eq!(up.started.lock().len(), 2, "max_attempts counts the hedges");
    let mut up = timed(&[slow("a"), slow("b")]);
    drive(vec![vec!["a", "b"]], hedged(30, 2), &mut up, 1).await;
    assert_eq!(up.started.lock().len(), 1, "one attempt allowed: no race");
}

#[tokio::test]
async fn a_hedge_crosses_into_the_next_tier() {
    let mut up = timed(&[("a", 1_000, Class::Success), ("b", 5, Class::Success)]);
    let out = drive(vec![vec!["a"], vec!["b"]], hedged(30, 1), &mut up, 5).await;
    assert!(matches!(out, Outcome::Success(ref d) if d == "b"));
}

use std::collections::{HashMap, VecDeque};

use antirion_core::{Stage, SystemClock};
use antirion_error::Message;

use super::*;

struct Script {
    tiers: Vec<Vec<&'static str>>,
    tier: usize,
    pos: usize,
    keys: HashMap<&'static str, Vec<&'static str>>,
    policy: Arc<Policy>,
}

impl Script {
    fn new(tiers: Vec<Vec<&'static str>>, policy: Policy) -> Script {
        Script {
            tiers,
            tier: 0,
            pos: 0,
            keys: HashMap::new(),
            policy: Arc::new(policy),
        }
    }
}

impl Planner for Script {
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

    fn select_key(&mut self, s: &Selected, tried: &[String], last: Option<&str>, mode: KeyMode) -> KeyPick {
        let Some(keys) = self.keys.get(s.deployment.as_str()) else {
            return KeyPick::Keyless;
        };
        match keys.iter().find(|k| !tried.iter().any(|t| t == *k)) {
            Some(k) => KeyPick::Key((*k).to_owned()),
            None if mode == KeyMode::PreferUntried => {
                last.map_or(KeyPick::None, |l| KeyPick::Key(l.to_owned()))
            }
            None => KeyPick::None,
        }
    }

    fn skip(&mut self, _s: &Selected, _reason: &'static str) {}

    async fn gate(&mut self, _s: &Selected) -> Result<(), &'static str> {
        Ok(())
    }
}

/// One attempt as logged: the deployment, its key, its class and whether it waited to start.
type Logged = (String, Option<String>, Class, bool);

/// Answers each attempt with the next scripted class of its deployment, success when none is left.
struct Upstreams {
    script: parking_lot::Mutex<HashMap<&'static str, VecDeque<Class>>>,
    log: parking_lot::Mutex<Vec<Logged>>,
}

impl Sender for Upstreams {
    type Done = ();

    async fn send(&self, a: Attempt<'_>) -> Sent<()> {
        let class = self
            .script
            .lock()
            .get_mut(a.selected.deployment.as_str())
            .and_then(VecDeque::pop_front)
            .unwrap_or(Class::Success);
        self.log.lock().push((
            a.selected.deployment.clone(),
            a.key.map(str::to_owned),
            class,
            !a.wait.is_zero(),
        ));
        if class == Class::Success {
            return Sent::Success(());
        }
        let mut error = GatewayError::new(class.final_code(), Stage::Attempt, Message::Fixed("upstream"));
        error.attempts = u16::try_from(a.index + 1).unwrap_or(u16::MAX);
        Sent::Failed(Failure {
            class,
            status: None,
            timer: None,
            hint: None,
            error: Box::new(error),
        })
    }
}

fn fast() -> Policy {
    let mut p = Policy::default();
    p.retry.backoff.initial = Duration::from_millis(1);
    p.retry.backoff.max = Duration::from_millis(2);
    p
}

async fn drive(plan: &mut Script, up: &mut Upstreams, max_attempts: u32) -> Outcome<()> {
    let health = HealthRegistry::default();
    let cfg = HealthConfig::default();
    let cancel = CancellationToken::new();
    run(
        plan,
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

fn upstreams(script: &[(&'static str, &[Class])]) -> Upstreams {
    Upstreams {
        script: parking_lot::Mutex::new(
            script
                .iter()
                .map(|(d, c)| (*d, c.iter().copied().collect()))
                .collect(),
        ),
        log: parking_lot::Mutex::new(Vec::new()),
    }
}

#[tokio::test]
async fn a_single_deployment_is_tried_three_times_by_default() {
    let mut plan = Script::new(vec![vec!["a"]], fast());
    let mut up = upstreams(&[("a", &[Class::ServerError; 5])]);
    let out = drive(&mut plan, &mut up, 5).await;
    assert!(matches!(out, Outcome::Failed(f) if f.class == Class::ServerError));
    assert_eq!(up.log.lock().len(), 3);
}

#[tokio::test]
async fn actions_go_rotate_key_then_sibling_then_retry_same() {
    let mut plan = Script::new(vec![vec!["a", "b"], vec!["c"]], fast());
    plan.keys.insert("a", vec!["k1", "k2"]);
    let mut up = upstreams(&[
        ("a", &[Class::RateLimited, Class::Overloaded]),
        ("b", &[Class::Overloaded]),
    ]);
    let out = drive(&mut plan, &mut up, 5).await;
    assert!(matches!(out, Outcome::Success(())));
    let log = up.log.lock();
    let got: Vec<(&str, Option<&str>, bool)> = log
        .iter()
        .map(|(d, k, _, w)| (d.as_str(), k.as_deref(), *w))
        .collect();
    assert_eq!(
        got,
        vec![
            ("a", Some("k1"), false),
            ("a", Some("k2"), false),
            ("b", None, false),
            ("b", None, true)
        ]
    );
}

#[tokio::test]
async fn caller_errors_end_the_loop_and_tiers_get_fresh_retries() {
    let mut plan = Script::new(vec![vec!["a"], vec!["b"]], fast());
    let mut up = upstreams(&[("a", &[Class::CallerError])]);
    assert!(
        matches!(drive(&mut plan, &mut up, 5).await, Outcome::Failed(f) if f.class == Class::CallerError)
    );
    assert_eq!(up.log.lock().len(), 1);

    let mut p = fast();
    p.retry.tier_retries = 1;
    let mut plan = Script::new(vec![vec!["a"], vec!["b"]], p);
    let mut up = upstreams(&[("a", &[Class::Overloaded; 5]), ("b", &[Class::Overloaded; 5])]);
    drive(&mut plan, &mut up, 10).await;
    let order: Vec<String> = up.log.lock().iter().map(|(d, ..)| d.clone()).collect();
    assert_eq!(order, vec!["a", "a", "b", "b"]);
}

#[tokio::test]
async fn max_attempts_caps_every_tier() {
    let mut p = fast();
    p.retry.tier_retries = 0;
    let tiers = vec![
        vec!["t0"],
        vec!["t1"],
        vec!["t2"],
        vec!["t3"],
        vec!["t4"],
        vec!["t5"],
    ];
    let mut plan = Script::new(tiers, p);
    let script: Vec<(&'static str, &[Class])> = ["t0", "t1", "t2", "t3", "t4", "t5"]
        .iter()
        .map(|d| (*d, &[Class::ServerError][..]))
        .collect();
    let mut up = upstreams(&script);
    drive(&mut plan, &mut up, 5).await;
    assert_eq!(up.log.lock().len(), 5);
}

/// Remembers whether each attempt was a repair, and switches to a fallback Model's tiers.
struct Fallbacks {
    inner: Script,
    fallback: Option<(&'static str, Vec<Vec<&'static str>>)>,
}

impl Planner for Fallbacks {
    fn select(&mut self, tried: &[String]) -> Option<Selected> {
        self.inner.select(tried)
    }
    fn next_tier(&mut self) -> bool {
        self.inner.next_tier()
    }
    fn attempted(&mut self, s: &Selected) {
        self.inner.attempted(s);
    }
    fn select_key(&mut self, s: &Selected, tried: &[String], last: Option<&str>, mode: KeyMode) -> KeyPick {
        self.inner.select_key(s, tried, last, mode)
    }
    fn skip(&mut self, s: &Selected, reason: &'static str) {
        self.inner.skip(s, reason);
    }
    async fn gate(&mut self, _s: &Selected) -> Result<(), &'static str> {
        Ok(())
    }
    fn class_fallback(&mut self, model: &str) -> bool {
        match self.fallback.take() {
            Some((name, tiers)) if name == model => {
                self.inner.tiers = tiers;
                self.inner.tier = 0;
                self.inner.pos = 0;
                true
            }
            _ => false,
        }
    }
}

#[tokio::test]
async fn a_class_fallback_walks_the_other_model_once() {
    let mut p = fast();
    p.retry.context_length_fallback = Some("big".into());
    let mut plan = Fallbacks {
        inner: Script::new(vec![vec!["small"], vec!["other"]], p),
        fallback: Some(("big", vec![vec!["large"]])),
    };
    let mut up = upstreams(&[
        ("small", &[Class::ContextLength]),
        ("large", &[Class::ContextLength]),
    ]);
    let health = HealthRegistry::default();
    let cfg = HealthConfig::default();
    let cancel = CancellationToken::new();
    let out = run(
        &mut plan,
        &mut up,
        LoopCx {
            health: &health,
            health_cfg: &cfg,
            clock: &SystemClock,
            cancel: &cancel,
            kind: RequestKind::Generation,
            max_attempts: 5,
            min_attempt_time: Duration::from_millis(1),
            deadline: None,
            fallback: true,
            seed: 1,
            read_timeout: None,
            ignore_health: &[],
        },
    )
    .await;
    assert!(matches!(out, Outcome::Failed(f) if f.class == Class::ContextLength));
    let order: Vec<String> = up.log.lock().iter().map(|(d, ..)| d.clone()).collect();
    assert_eq!(
        order,
        vec!["small", "large"],
        "one class fallback, no chaining, no other tier"
    );
}

#[tokio::test]
async fn a_rejected_reasoning_replay_is_repaired_once() {
    let mut plan = Script::new(vec![vec!["a"], vec!["b"]], fast());
    let mut up = upstreams(&[("a", &[Class::ReasoningReplayRejected; 3])]);
    let out = drive(&mut plan, &mut up, 5).await;
    assert!(matches!(out, Outcome::Failed(f) if f.class == Class::ReasoningReplayRejected));
    let order: Vec<String> = up.log.lock().iter().map(|(d, ..)| d.clone()).collect();
    assert_eq!(order, vec!["a", "a"]);
}

#[tokio::test]
async fn a_spent_retry_budget_moves_to_the_next_tier() {
    let health = HealthRegistry::default();
    let cfg = HealthConfig::from_doc(
        &serde_json::json!({"resilience": {"retry_budget": {"min_per_second": 0, "ratio": 0}}}),
    );
    let cancel = CancellationToken::new();
    let mut plan = Script::new(vec![vec!["a"], vec!["b"]], fast());
    let mut up = upstreams(&[("a", &[Class::ServerError; 3])]);
    let out = run(
        &mut plan,
        &mut up,
        LoopCx {
            health: &health,
            health_cfg: &cfg,
            clock: &SystemClock,
            cancel: &cancel,
            kind: RequestKind::Generation,
            max_attempts: 5,
            min_attempt_time: Duration::from_millis(1),
            deadline: None,
            fallback: true,
            seed: 1,
            read_timeout: None,
            ignore_health: &[],
        },
    )
    .await;
    assert!(matches!(out, Outcome::Success(())));
    let order: Vec<String> = up.log.lock().iter().map(|(d, ..)| d.clone()).collect();
    assert_eq!(
        order,
        vec!["a", "b"],
        "the retry of a is refused, the move to b is a first attempt"
    );
}

#[test]
fn the_read_timeout_caps_stream_timers_and_marks_them_shortened() {
    let p = Policy::default();
    let t = capped(&p.timeouts, Some(Duration::from_secs(1)));
    let timers = AttemptTimers {
        connect: t.connect,
        timeouts: t,
        configured: p.timeouts.clone(),
        deadline_at: None,
        deadline: None,
    };
    assert_eq!(timers.value(TimerKind::Ttft), Some(Duration::from_secs(1)));
    assert!(timers.shortened(TimerKind::Ttft));
    assert!(timers.shortened(TimerKind::StreamIdle));
    assert!(!timers.shortened(TimerKind::Attempt));
    assert!(!timers.shortened(TimerKind::Upstream));
    assert!(timers.shortened(TimerKind::Deadline));
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use antirion_core::{ResourceUid, UtcMicros};
use antirion_error::Code;
use antirion_testkit::FakeClock;
use proptest::prelude::*;
use serde_json::{Value, json};
use tokio::task::JoinSet;

use super::*;
use crate::applicable::{RequestAttrs, Scope};
use crate::cost::FixedUnits;
use crate::estimate::Estimate;
use crate::estimate::OutSource;
use crate::money::{Dec, Nusd};
use crate::plan::{AdmitInput, plan, plan_holds};
use crate::policy::Policies;
use crate::price::Price;
use crate::store::{AttemptRequest, NegativeSpend};
use crate::usage::Usage;
use crate::weights::Incurred;

/// A memory store that can be switched off.
#[derive(Debug)]
struct Flaky {
    inner: MemoryCounters,
    down: AtomicBool,
}

impl Flaky {
    fn check(&self) -> Result<(), StoreError> {
        if self.down.load(Ordering::SeqCst) {
            Err(StoreError::Unavailable("down".into()))
        } else {
            Ok(())
        }
    }
}

impl CounterStore for Flaky {
    async fn admit(&self, req: &AdmitRequest<'_>) -> Result<AdmitDecision, StoreError> {
        self.check()?;
        self.inner.admit(req).await
    }
    async fn admit_attempt(&self, req: &AttemptRequest<'_>) -> Result<AdmitDecision, StoreError> {
        self.check()?;
        self.inner.admit_attempt(req).await
    }
    async fn reconcile(&self, req: &SettleRequest<'_>) -> Result<SettleOutcome, StoreError> {
        self.check()?;
        self.inner.reconcile(req).await
    }
    async fn heartbeat(&self, node: &str, ttl_us: u64) -> Result<(), StoreError> {
        self.check()?;
        self.inner.heartbeat(node, ttl_us).await
    }
    async fn reap(&self, max: usize) -> Result<Vec<ExpiredReservation>, StoreError> {
        self.check()?;
        self.inner.reap(max).await
    }
    async fn read(&self, holds: &[Hold]) -> Result<Vec<CounterView>, StoreError> {
        self.check()?;
        self.inner.read(holds).await
    }
    async fn seed_budget(&self, hold: &Hold, spent: u128) -> Result<(), StoreError> {
        self.check()?;
        self.inner.seed_budget(hold, spent).await
    }
    async fn seed_window(&self, hold: &Hold, slots: &[(i64, u128)]) -> Result<(), StoreError> {
        self.check()?;
        self.inner.seed_window(hold, slots).await
    }
    async fn adjust_budget(
        &self,
        hold: &Hold,
        delta: i128,
    ) -> Result<Result<u128, NegativeSpend>, StoreError> {
        self.check()?;
        self.inner.adjust_budget(hold, delta).await
    }
    async fn ping(&self) -> Result<(), StoreError> {
        self.check()
    }
}

const T0: UtcMicros = UtcMicros(1_790_000_000_000_000);

fn engine() -> LimitsEngine<Flaky> {
    let clock = Arc::new(FakeClock::at(T0));
    let store = Flaky {
        inner: MemoryCounters::new(clock.clone()),
        down: AtomicBool::new(false),
    };
    LimitsEngine::new(
        store,
        clock,
        EngineConfig {
            node: "n1".into(),
            lease_ttl: Duration::from_secs(60),
            store_timeout: Duration::from_millis(100),
            replay_capacity: 100,
        },
    )
}

fn policies(doc: Value) -> Policies {
    Policies::compile(&doc, &|_, n| ResourceUid(n.len() as u128 + 1)).unwrap()
}

fn input<'a>(key: &'a str, out: u64, candidates: &'a [Option<&'a Price>]) -> AdmitInput<'a> {
    AdmitInput {
        uid: ReservationId(ResourceUid::random().0),
        attrs: RequestAttrs {
            organization: "default",
            team: "t1",
            project: "default",
            key,
            model: Some("m"),
            endpoint: "chat_completions",
            ..RequestAttrs::default()
        },
        estimate: Estimate {
            input: 20,
            output: out,
            out_source: OutSource::Request,
            clamp_to: None,
        },
        hedge: 1,
        incurs: Incurred::default(),
        candidates,
        fixed: FixedUnits::default(),
        multiplier: Dec::ONE,
        now: T0,
    }
}

fn settled(output: u64, billed: i128) -> Settlement {
    Settlement {
        usage: Usage {
            input: 20,
            output,
            ..Usage::default()
        },
        billed: Nusd(billed),
        success: true,
    }
}

#[tokio::test]
async fn refunds_readmit_and_headers_show_the_state() {
    let e = engine();
    let p = policies(json!({"keys": {"k": {"policies": {"limits": {
        "tpd": {"unit": "tokens", "tokens": "output", "limit": 1000, "per": "1d"},
        "rpm": {"unit": "requests", "limit": 10, "per": "60s"}
    }}}}}));
    let mut first = Vec::new();
    for _ in 0..3 {
        first.push(e.admit(plan(&p, &input("k", 300, &[])).unwrap()).await.unwrap());
    }
    let err = e
        .admit(plan(&p, &input("k", 300, &[])).unwrap())
        .await
        .unwrap_err();
    assert_eq!(err.code, Code::RateLimited);
    assert!(
        err.extra_headers
            .as_ref()
            .is_some_and(|h| h.contains_key("ratelimit"))
    );
    let (policy, current) = first[0].ietf_headers().unwrap();
    assert!(policy.contains("\"key.rpm\";q=10;w=60"));
    assert!(current.contains("\"key.rpm\";r=9;t=6"));
    for r in &first {
        e.settle(r, &settled(10, 0)).await;
    }
    for _ in 0..3 {
        e.admit(plan(&p, &input("k", 300, &[])).unwrap()).await.unwrap();
    }
}

#[tokio::test]
async fn refused_requests_hold_nothing() {
    let e = engine();
    let p = policies(json!({
        "teams": {"t1": {"organization": "default", "policies": {"limits": {"rph": {"unit": "requests", "limit": 3, "per": "1h"}}}}},
        "keys": {"k1": {"policies": {"limits": {"rph": {"unit": "requests", "limit": 5, "per": "1h"}}}}}
    }));
    for _ in 0..3 {
        e.admit(plan(&p, &input("k1", 1, &[])).unwrap()).await.unwrap();
    }
    let refused = e
        .admit(plan(&p, &input("k1", 1, &[])).unwrap())
        .await
        .unwrap_err();
    assert_eq!(refused.limit.as_ref().unwrap()["level"], "team");
    let k2 = e
        .admit(plan(&p, &input("k2", 1, &[])).unwrap())
        .await
        .unwrap_err();
    assert_eq!(k2.limit.as_ref().unwrap()["level"], "team");
    let k1 = plan(&p, &input("k1", 1, &[])).unwrap();
    let views = e.store().read(&k1.store_holds()).await.unwrap();
    let key_view = k1
        .holds
        .iter()
        .zip(&views)
        .find(|(h, _)| h.info.level == crate::level::Level::Key)
        .unwrap()
        .1;
    assert_eq!(key_view.remaining, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn usd_budget_admits_exactly_and_settles_to_the_ledger_amount() {
    let e = Arc::new(engine());
    let p = Arc::new(policies(
        json!({"keys": {"k": {"policies": {"budgets": {"b": {"unit": "usd", "amount": "3.50"}}}}}}),
    ));
    let price = Arc::new(Price::parse(&json!({"input": "0", "output": "10000.00"})).unwrap());
    let mut set = JoinSet::new();
    for _ in 0..20 {
        let (e, p, price) = (Arc::clone(&e), Arc::clone(&p), Arc::clone(&price));
        set.spawn(async move {
            let cands = [Some(&*price)];
            e.admit(plan(&p, &input("k", 100, &cands)).unwrap()).await
        });
    }
    let mut admitted = Vec::new();
    while let Some(res) = set.join_next().await {
        match res.unwrap() {
            Ok(r) => admitted.push(r),
            Err(err) => {
                assert_eq!(err.code, Code::BudgetExceeded);
                assert_eq!(err.retry_after, None);
            }
        }
    }
    assert_eq!(admitted.len(), 3);
    assert_eq!(admitted[0].usd, Some(Nusd(1_000_000_000)));
    for r in &admitted {
        e.settle(r, &settled(10, 100_000_000)).await;
    }
    let price = Price::parse(&json!({"input": "0", "output": "10000.00"})).unwrap();
    let cands = [Some(&price)];
    let b = plan(&p, &input("k", 100, &cands)).unwrap();
    let v = e.store().read(&b.store_holds()).await.unwrap()[0];
    assert_eq!((v.spent, v.reserved), (300_000_000, 0));
}

#[tokio::test]
async fn store_down_local_share_fail_closed_and_fail_open() {
    let e = engine();
    e.set_known_nodes(2);
    e.store().down.store(true, Ordering::SeqCst);
    let p = policies(
        json!({"keys": {"k": {"policies": {"limits": {"rph": {"unit": "requests", "limit": 10, "per": "1h"}}}}}}),
    );
    let mut admitted = 0;
    for _ in 0..20 {
        if let Ok(r) = e.admit(plan(&p, &input("k", 1, &[])).unwrap()).await {
            assert_eq!(r.degraded.limits, Some(DegradedCause::LocalShare));
            admitted += 1;
        }
    }
    assert_eq!(admitted, 5);
    let closed = policies(
        json!({"keys": {"k": {"policies": {"limits": {"c": {"unit": "requests", "limit": 10, "per": "1h", "on_store_unavailable": "fail_closed"}}}}}}),
    );
    let err = e
        .admit(plan(&closed, &input("k", 1, &[])).unwrap())
        .await
        .unwrap_err();
    assert_eq!(
        (err.code, err.reason.unwrap().as_str()),
        (Code::DependencyUnavailable, "limits_unavailable")
    );
    assert_eq!(err.retry_after, Some(Duration::from_secs(1)));
    let budget = policies(
        json!({"keys": {"k": {"policies": {"budgets": {"b": {"unit": "tokens", "amount": 1000}}}}}}),
    );
    assert_eq!(
        e.admit(plan(&budget, &input("k", 1, &[])).unwrap())
            .await
            .unwrap_err()
            .code,
        Code::DependencyUnavailable
    );
    let open = policies(
        json!({"keys": {"k": {"policies": {"limits": {"o": {"unit": "requests", "limit": 0, "per": "1h", "on_store_unavailable": "fail_open"}}}}}}),
    );
    let r = e.admit(plan(&open, &input("k", 1, &[])).unwrap()).await.unwrap();
    assert_eq!(r.degraded.limits, Some(DegradedCause::FailOpen));
    let conc = policies(
        json!({"keys": {"k": {"policies": {"limits": {"c": {"unit": "concurrency", "limit": 1}}}}}}),
    );
    let err = e
        .admit(plan(&conc, &input("k", 1, &[])).unwrap())
        .await
        .unwrap_err();
    assert_eq!(err.code, Code::RateLimited);
    assert_eq!(err.retry_after, Some(Duration::from_secs(1)));
}

#[tokio::test]
async fn settlements_queue_while_down_and_replay_once() {
    let e = engine();
    let p = policies(
        json!({"keys": {"k": {"policies": {"limits": {"c": {"unit": "concurrency", "limit": 1}}}}}}),
    );
    let r = e.admit(plan(&p, &input("k", 1, &[])).unwrap()).await.unwrap();
    e.store().down.store(true, Ordering::SeqCst);
    let report = e.settle(&r, &settled(1, 0)).await;
    assert!(report.queued);
    assert_eq!(report.degraded.limits, Some(DegradedCause::StoreTimeout));
    assert_eq!(e.flush().await, 0);
    e.store().down.store(false, Ordering::SeqCst);
    assert!(!e.probe().await && !e.probe().await && e.probe().await);
    assert_eq!(e.flush().await, 1);
    assert_eq!(e.queued(), (0, 0));
    let again = e.admit(plan(&p, &input("k", 1, &[])).unwrap()).await.unwrap();
    assert!(again.remote);
}

#[tokio::test]
async fn soft_budgets_alert_once_per_threshold() {
    let e = engine();
    let p = policies(
        json!({"keys": {"k": {"policies": {"budgets": {"s": {"unit": "usd", "amount": "0.05", "mode": "soft", "alert_at": [0.5, 1.0]}}}}}}),
    );
    let price = Price::parse(&json!({"input": "0", "output": "300.00"})).unwrap();
    let cands = [Some(&price)];
    let mut alerts = Vec::new();
    for _ in 0..5 {
        let r = e
            .admit(plan(&p, &input("k", 100, &cands)).unwrap())
            .await
            .unwrap();
        alerts.extend(e.settle(&r, &settled(100, 30_000_000)).await.alerts);
    }
    let thresholds: Vec<String> = alerts.iter().map(|a| a.threshold.to_string()).collect();
    assert_eq!(thresholds, vec!["0.5", "1"]);
    assert_eq!(alerts[1].spent, 60_000_000);
}

#[tokio::test]
async fn attempt_holds_make_a_position_ineligible() {
    let e = engine();
    let p = policies(
        json!({"deployments": {"a": {"provider": "p", "model": "x", "limits": {"rph": {"unit": "requests", "limit": 1, "per": "1h"}}}}}),
    );
    let i = input("k", 1, &[]);
    let mut r = e.admit(plan(&p, &i).unwrap()).await.unwrap();
    let nodes: Vec<String> = vec![];
    let (holds, _) = plan_holds(
        &p,
        &i,
        Scope::Attempt {
            nodes: &nodes,
            deployment: Some("a"),
        },
    )
    .unwrap();
    e.admit_attempt(&mut r, 1, holds.clone()).await.unwrap();
    assert!(
        r.remote && !r.degraded.is_degraded(),
        "the first attempt's holds open the reservation in the store"
    );
    let i2 = input("k", 1, &[]);
    let mut r2 = e.admit(plan(&p, &i2).unwrap()).await.unwrap();
    let refused = e.admit_attempt(&mut r2, 1, holds).await.unwrap_err();
    assert!(refused.wait.is_some_and(|w| w > Duration::from_secs(3500)));
    assert_eq!(refused.error.limit.as_ref().unwrap()["level"], "deployment");
    e.settle(&r, &settled(1, 0)).await;
    e.settle(&r2, &settled(1, 0)).await;
}

#[tokio::test]
async fn empty_plans_make_no_store_call() {
    let e = engine();
    e.store().down.store(true, Ordering::SeqCst);
    let p = policies(json!({}));
    let r = e.admit(plan(&p, &input("k", 1, &[])).unwrap()).await.unwrap();
    assert!(!r.remote && !r.degraded.is_degraded());
    assert!(!e.is_store_down());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// Strict output and usage within the reservation: spend never passes the quota, whatever the
    /// interleaving of admissions and settlements.
    #[test]
    fn concurrent_spend_never_exceeds_the_quota(jobs in proptest::collection::vec((1u64..200, 0u64..200), 1..120)) {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();
        let (tokens, usd) = rt.block_on(async move {
            let e = Arc::new(engine());
            let p = Arc::new(policies(json!({"keys": {"k": {"policies": {
                "limits": {"t": {"unit": "tokens", "tokens": "output", "limit": 5000, "per": "1d"}},
                "budgets": {"b": {"unit": "usd", "amount": "20.00"}}
            }}}})));
            let price = Arc::new(Price::parse(&json!({"input": "0", "output": "10000.00"})).unwrap());
            let mut set = JoinSet::new();
            for (max, completion) in jobs {
                let (e, p, price) = (Arc::clone(&e), Arc::clone(&p), Arc::clone(&price));
                set.spawn(async move {
                    let out = completion.min(max);
                    let cands = [Some(&*price)];
                    let plan = plan(&p, &input("k", max, &cands)).unwrap();
                    match e.admit(plan).await {
                        Ok(r) => {
                            let billed = i128::from(out) * 10_000_000;
                            e.settle(&r, &settled(out, billed)).await;
                            (u128::from(out), billed)
                        }
                        Err(_) => (0, 0),
                    }
                });
            }
            let (mut t, mut u) = (0u128, 0i128);
            while let Some(res) = set.join_next().await {
                let (a, b) = res.unwrap();
                t += a;
                u += b;
            }
            (t, u)
        });
        prop_assert!(tokens <= 5000);
        prop_assert!(usd <= 20_000_000_000);
    }
}

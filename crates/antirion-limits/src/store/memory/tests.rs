use std::sync::Arc;
use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_testkit::FakeClock;
use proptest::prelude::*;
use tokio::task::JoinSet;

use super::MemoryCounters;
use crate::counter::HoldMode;
use crate::store::scenarios::{self, HOUR_US, admit, admitted, budget, conc, gcra, id, uid};
use crate::store::{CounterStore, ResvState, SettleOutcome, SettleRequest};

fn store() -> (Arc<FakeClock>, MemoryCounters) {
    let clock = Arc::new(FakeClock::at(UtcMicros(1_790_000_000_000_000)));
    let s = MemoryCounters::new(clock.clone());
    (clock, s)
}

#[tokio::test]
async fn shared_scenarios() {
    let (_, s) = store();
    scenarios::all_or_nothing(&s).await;
    scenarios::idempotency(&s).await;
    scenarios::refunds_readmit(&s).await;
    scenarios::budgets(&s).await;
    scenarios::rolling_budgets(&s, 1_790_000_000_000_000).await;
    scenarios::headroom(&s).await;
    scenarios::concurrency_and_attempts(&s).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_admissions_never_overshoot() {
    let (_, s) = store();
    scenarios::concurrent_no_overshoot(Arc::new(s), 60).await;
}

#[tokio::test]
async fn refill_follows_the_clock() {
    let (clock, s) = store();
    let h = gcra(id("rps"), 3, 1_500_000, 1);
    for _ in 0..3 {
        assert!(admitted(&admit(&s, uid(), std::slice::from_ref(&h)).await));
    }
    assert!(!admitted(&admit(&s, uid(), std::slice::from_ref(&h)).await));
    clock.advance(Duration::from_millis(1500));
    for _ in 0..3 {
        assert!(admitted(&admit(&s, uid(), std::slice::from_ref(&h)).await));
    }
    assert!(!admitted(&admit(&s, uid(), std::slice::from_ref(&h)).await));
}

#[tokio::test]
async fn expired_leases_release_concurrency_and_charge_holds() {
    let (clock, s) = store();
    s.heartbeat("n1", 6_000_000).await.unwrap();
    let c = conc(id("conc"), 1);
    let b = budget(id("b"), 10, 4);
    let mut soft = budget(id("soft"), 10, 3);
    soft.mode = HoldMode::Track;
    let u = uid();
    assert!(admitted(
        &admit(&s, u, &[c.clone(), b.clone(), soft.clone()]).await
    ));
    assert!(!admitted(&admit(&s, uid(), std::slice::from_ref(&c)).await));
    clock.advance(Duration::from_secs(5));
    assert!(s.reap(10).await.unwrap().is_empty());
    s.heartbeat("n1", 6_000_000).await.unwrap();
    clock.advance(Duration::from_secs(5));
    assert!(s.reap(10).await.unwrap().is_empty());
    clock.advance(Duration::from_secs(2));
    let expired = s.reap(10).await.unwrap();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].uid, u);
    assert!(s.reap(10).await.unwrap().is_empty());
    assert!(admitted(&admit(&s, uid(), std::slice::from_ref(&c)).await));
    let views = s.read(&[b.clone(), soft.clone()]).await.unwrap();
    assert_eq!((views[0].spent, views[0].reserved), (4, 0));
    assert_eq!(views[1].spent, 3);
    assert_eq!(
        scenarios::settle(&s, u, &[c.clone(), b.clone(), soft.clone()], &[0, 0, 0])
            .await
            .unwrap(),
        SettleOutcome::Noop(Some(ResvState::Expired))
    );
}

#[tokio::test]
async fn sweep_keeps_drained_counters_and_forgets_closed_records() {
    let (clock, s) = store();
    let h = gcra(id("keep"), 2, HOUR_US, 1);
    let u = uid();
    assert!(admitted(&admit(&s, u, std::slice::from_ref(&h)).await));
    s.reconcile(&SettleRequest {
        uid: u,
        node: "n1",
        group: 7,
        holds: std::slice::from_ref(&h),
        actual: &[1],
        keep_us: 1_000_000,
    })
    .await
    .unwrap();
    clock.advance(Duration::from_secs(2));
    s.sweep();
    assert_eq!(s.read(std::slice::from_ref(&h)).await.unwrap()[0].remaining, 1);
    assert!(admitted(&admit(&s, u, std::slice::from_ref(&h)).await));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn concurrent_reservations_admit_exactly_floor_of_quota(
        quota in 1u64..2000, rho in 1u128..400, tasks in 1usize..80, use_budget in any::<bool>(),
    ) {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).build().unwrap();
        let admitted_total = rt.block_on(async move {
            let (_, s) = store();
            let s = Arc::new(s);
            let h = if use_budget { budget(id("p"), u128::from(quota), rho) } else { gcra(id("p"), quota, 86_400_000_000, rho) };
            let mut set = JoinSet::new();
            for _ in 0..tasks {
                let s = Arc::clone(&s);
                let h = h.clone();
                set.spawn(async move { admitted(&admit(&*s, uid(), &[h]).await) });
            }
            let mut n = 0u128;
            while let Some(ok) = set.join_next().await {
                if ok.unwrap() {
                    n += 1;
                }
            }
            n
        });
        let expect = (u128::from(quota) / rho).min(tasks as u128);
        prop_assert_eq!(admitted_total, expect);
    }

    #[test]
    fn settlements_are_exact(rhos in proptest::collection::vec((1u128..300, 0u128..300), 1..20)) {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async move {
            let (_, s) = store();
            let b = budget(id("x"), u128::MAX / 4, 0);
            let mut spent = 0u128;
            for (rho, actual) in rhos {
                let mut h = b.clone();
                h.amount = rho;
                let u = uid();
                assert!(admitted(&admit(&s, u, &[h]).await));
                scenarios::settle(&s, u, &[], &[actual]).await.unwrap();
                spent += actual;
            }
            let v = s.read(&[b]).await.unwrap()[0];
            assert_eq!((v.spent, v.reserved), (spent, 0));
        });
    }
}

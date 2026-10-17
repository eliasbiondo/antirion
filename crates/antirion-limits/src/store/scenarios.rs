//! Behaviour every counter store must show, run against each backend by its own tests.

use std::sync::Arc;

use antirion_core::{ResourceUid, UtcMicros};
use tokio::task::JoinSet;

use super::{
    AdmitDecision, AdmitRequest, AttemptRequest, CounterStore, CounterView, RefusalKind, ReservationId,
    ResvState, SettleOutcome, SettleRequest,
};
use crate::counter::{CounterId, Hold, HoldMode, Rule};
use crate::level::Level;
use crate::math::{Gcra, Window};
use crate::period::PeriodBounds;

pub(crate) const HOUR_US: u64 = 3_600_000_000;
pub(crate) const DAY_US: u64 = 86_400_000_000;

/// A fresh counter id per test run, so tests sharing a store never share counters.
pub(crate) fn id(tag: &str) -> CounterId {
    let salt = ResourceUid::random().to_hex();
    CounterId::derive("test", Level::Key, ResourceUid(1), &salt, tag, "", &[])
}

pub(crate) fn uid() -> ReservationId {
    ReservationId(ResourceUid::random().0)
}

pub(crate) fn gcra(counter: CounterId, limit: u64, per_us: u64, rho: u128) -> Hold {
    Hold {
        counter,
        group: 0,
        rule: Rule::Gcra(Gcra {
            limit,
            per_us,
            burst: limit,
        }),
        amount: rho,
        mode: HoldMode::Enforce,
    }
}

pub(crate) fn window(counter: CounterId, limit: u64, per_us: u64, precision: u32, rho: u128) -> Hold {
    Hold {
        counter,
        group: 0,
        rule: Rule::Window(Window {
            limit,
            per_us,
            precision,
        }),
        amount: rho,
        mode: HoldMode::Enforce,
    }
}

pub(crate) fn conc(counter: CounterId, limit: u64) -> Hold {
    Hold {
        counter,
        group: 0,
        rule: Rule::Concurrency { limit },
        amount: 1,
        mode: HoldMode::Enforce,
    }
}

pub(crate) fn budget(counter: CounterId, amount: u128, rho: u128) -> Hold {
    Hold {
        counter,
        group: 7,
        rule: Rule::Budget {
            amount,
            period: PeriodBounds {
                start: UtcMicros::EPOCH,
                end: None,
            },
        },
        amount: rho,
        mode: HoldMode::Enforce,
    }
}

pub(crate) async fn admit<S: CounterStore>(s: &S, u: ReservationId, holds: &[Hold]) -> AdmitDecision {
    s.admit(&AdmitRequest {
        uid: u,
        node: "n1",
        group: 7,
        holds,
    })
    .await
    .unwrap()
}

pub(crate) async fn settle<S: CounterStore>(
    s: &S,
    u: ReservationId,
    holds: &[Hold],
    actual: &[u128],
) -> Result<SettleOutcome, super::StoreError> {
    s.reconcile(&SettleRequest {
        uid: u,
        node: "n1",
        group: 7,
        holds,
        actual,
        keep_us: 60_000_000,
    })
    .await
}

pub(crate) async fn attempt<S: CounterStore>(s: &S, u: ReservationId, n: u16, h: &Hold) -> AdmitDecision {
    s.admit_attempt(&AttemptRequest {
        uid: u,
        group: 7,
        attempt: n,
        holds: std::slice::from_ref(h),
    })
    .await
    .unwrap()
}

pub(crate) fn admitted(d: &AdmitDecision) -> bool {
    matches!(d, AdmitDecision::Admitted { .. })
}

async fn view<S: CounterStore>(s: &S, h: &Hold) -> CounterView {
    s.read(std::slice::from_ref(h)).await.unwrap()[0]
}

pub(crate) async fn all_or_nothing<S: CounterStore>(s: &S) {
    let team = gcra(id("team"), 3, HOUR_US, 1);
    let k1 = gcra(id("k1"), 5, HOUR_US, 1);
    let k2 = gcra(id("k2"), 5, HOUR_US, 1);
    for _ in 0..3 {
        assert!(admitted(&admit(s, uid(), &[team.clone(), k1.clone()]).await));
    }
    let refused = admit(s, uid(), &[team.clone(), k1.clone()]).await;
    let AdmitDecision::Refused { refusals } = refused else {
        panic!("{refused:?}")
    };
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].index, 0);
    assert_eq!(refusals[0].kind, RefusalKind::Rate);
    assert!(refusals[0].wait_us.is_some_and(|w| w > 0));
    assert!(!admitted(&admit(s, uid(), &[team.clone(), k2.clone()]).await));
    assert_eq!(view(s, &k1).await.remaining, 2);
    assert_eq!(view(s, &k2).await.remaining, 5);
    let exceeds = admit(s, uid(), &[gcra(id("tiny"), 100, DAY_US, 500)]).await;
    let AdmitDecision::Refused { refusals } = exceeds else {
        panic!()
    };
    assert_eq!(refusals[0].kind, RefusalKind::Exceeds);
}

pub(crate) async fn idempotency<S: CounterStore>(s: &S) {
    let h = gcra(id("idem"), 2, HOUR_US, 1);
    let u = uid();
    let first = admit(s, u, std::slice::from_ref(&h)).await;
    let AdmitDecision::Admitted { views } = first else {
        panic!()
    };
    assert_eq!(views[0].remaining, 1);
    assert_eq!(
        admit(s, u, std::slice::from_ref(&h)).await,
        AdmitDecision::Replayed(ResvState::Held)
    );
    assert_eq!(view(s, &h).await.remaining, 1);
    assert!(matches!(
        settle(s, u, std::slice::from_ref(&h), &[1]).await.unwrap(),
        SettleOutcome::Settled { .. }
    ));
    assert_eq!(
        settle(s, u, std::slice::from_ref(&h), &[1]).await.unwrap(),
        SettleOutcome::Noop(Some(ResvState::Settled))
    );
    assert_eq!(view(s, &h).await.remaining, 1);
    let late = uid();
    assert_eq!(
        settle(s, late, &[], &[]).await.unwrap(),
        SettleOutcome::Noop(None)
    );
    assert_eq!(
        admit(s, late, std::slice::from_ref(&h)).await,
        AdmitDecision::Replayed(ResvState::Cancelled)
    );
    assert_eq!(view(s, &h).await.remaining, 1);
}

pub(crate) async fn refunds_readmit<S: CounterStore>(s: &S) {
    let c = id("tpd");
    let h = gcra(c, 1000, DAY_US, 300);
    let ids: Vec<_> = (0..3).map(|_| uid()).collect();
    for u in &ids {
        assert!(admitted(&admit(s, *u, std::slice::from_ref(&h)).await));
    }
    assert!(!admitted(&admit(s, uid(), std::slice::from_ref(&h)).await));
    for u in &ids {
        settle(s, *u, std::slice::from_ref(&h), &[10]).await.unwrap();
    }
    assert_eq!(view(s, &h).await.remaining, 970);
    for _ in 0..3 {
        assert!(admitted(&admit(s, uid(), std::slice::from_ref(&h)).await));
    }
    let w = window(id("win"), 5, 10_000_000, 10, 1);
    for _ in 0..5 {
        assert!(admitted(&admit(s, uid(), std::slice::from_ref(&w)).await));
    }
    let AdmitDecision::Refused { refusals } = admit(s, uid(), std::slice::from_ref(&w)).await else {
        panic!()
    };
    assert!(
        refusals[0]
            .wait_us
            .is_some_and(|us| us > 9_000_000 && us <= 11_000_000)
    );
}

pub(crate) async fn budgets<S: CounterStore>(s: &S) {
    let c = id("usd");
    let rho = 1_000_000_000u128;
    let b = budget(c, 3_500_000_000, rho);
    let ids: Vec<_> = (0..3).map(|_| uid()).collect();
    for u in &ids {
        assert!(admitted(&admit(s, *u, std::slice::from_ref(&b)).await));
    }
    let AdmitDecision::Refused { refusals } = admit(s, uid(), std::slice::from_ref(&b)).await else {
        panic!()
    };
    assert_eq!(refusals[0].kind, RefusalKind::Budget);
    let mut last = 0;
    for u in &ids {
        let SettleOutcome::Settled { budgets } = settle(s, *u, std::slice::from_ref(&b), &[100_000_000])
            .await
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(budgets.len(), 1);
        assert_eq!(budgets[0].before, last);
        last = budgets[0].after;
    }
    let v = view(s, &b).await;
    assert_eq!(
        (v.spent, v.reserved, v.remaining),
        (300_000_000, 0, 3_200_000_000)
    );
    assert_eq!(
        s.adjust_budget(&b, -400_000_000).await.unwrap(),
        Err(super::NegativeSpend)
    );
    assert_eq!(
        s.adjust_budget(&b, 3_200_000_000).await.unwrap(),
        Ok(3_500_000_000)
    );
    let AdmitDecision::Refused { .. } = admit(s, uid(), std::slice::from_ref(&b)).await else {
        panic!()
    };
    let mut free = b.clone();
    free.mode = HoldMode::Track;
    assert!(admitted(&admit(s, uid(), &[free]).await));
    s.seed_budget(&b, 0).await.unwrap();
    assert!(
        !admitted(&admit(s, uid(), std::slice::from_ref(&b)).await),
        "seeding never lowers spend"
    );
    assert_eq!(s.adjust_budget(&b, -3_500_000_000).await.unwrap(), Ok(0));
    assert!(admitted(&admit(s, uid(), std::slice::from_ref(&b)).await));
    let zero = budget(id("zero"), 0, 1);
    assert!(!admitted(&admit(s, uid(), &[zero]).await));
}

/// A rolling budget of a day in hourly sub-windows: it refuses like a budget, settles to the billed
/// amount, reports its unexpired charges as spent, takes seeded sub-windows and adjustments.
pub(crate) async fn rolling_budgets<S: CounterStore>(s: &S, now: i64) {
    let rolling = |counter, rho| Hold {
        counter,
        group: 7,
        rule: Rule::Rolling(Window {
            limit: 1_000,
            per_us: DAY_US,
            precision: 24,
        }),
        amount: rho,
        mode: HoldMode::Enforce,
    };
    let b = rolling(id("rolling"), 400);
    let (u1, u2) = (uid(), uid());
    assert!(admitted(&admit(s, u1, std::slice::from_ref(&b)).await));
    assert!(admitted(&admit(s, u2, std::slice::from_ref(&b)).await));
    let AdmitDecision::Refused { refusals } = admit(s, uid(), std::slice::from_ref(&b)).await else {
        panic!()
    };
    assert_eq!(refusals[0].kind, RefusalKind::Budget);
    assert!(
        refusals[0].wait_us.is_some(),
        "a rolling budget frees spend as charges expire"
    );
    let SettleOutcome::Settled { budgets } = settle(s, u1, std::slice::from_ref(&b), &[100]).await.unwrap()
    else {
        panic!()
    };
    assert_eq!((budgets[0].before, budgets[0].after), (800, 500));
    let v = view(s, &b).await;
    assert_eq!((v.spent, v.remaining), (500, 500));
    let mut soft = b.clone();
    soft.mode = HoldMode::Track;
    let u3 = uid();
    assert!(admitted(&admit(s, u3, std::slice::from_ref(&soft)).await));
    settle(s, u3, std::slice::from_ref(&soft), &[50]).await.unwrap();
    assert_eq!(
        view(s, &b).await.spent,
        550,
        "a soft rolling budget is charged at settlement"
    );
    assert_eq!(
        s.adjust_budget(&b, -600).await.unwrap(),
        Err(super::NegativeSpend)
    );
    assert_eq!(s.adjust_budget(&b, -50).await.unwrap(), Ok(500));
    let fresh = rolling(id("seeded"), 1);
    let Rule::Rolling(w) = fresh.rule else { panic!() };
    s.seed_window(&fresh, &[(w.slot(now), 900), (w.slot(now) - 30, 999)])
        .await
        .unwrap();
    assert_eq!(
        view(s, &fresh).await.spent,
        900,
        "a seeded sub-window that expired is dropped"
    );
    s.seed_window(&fresh, &[(w.slot(now), 100)]).await.unwrap();
    assert_eq!(
        view(s, &fresh).await.spent,
        900,
        "seeding never lowers a sub-window"
    );
}

/// An enforced hold keeps free what the headroom holds after it have left (priority classes under
/// `borrow`); a headroom hold is never charged, at admission or settlement.
pub(crate) async fn headroom<S: CounterStore>(s: &S) {
    let total = window(id("total"), 10, HOUR_US, 60, 5);
    let mut peer = window(id("peer"), 6, HOUR_US, 60, 0);
    peer.mode = HoldMode::Headroom;
    let AdmitDecision::Refused { refusals } = admit(s, uid(), &[total.clone(), peer.clone()]).await else {
        panic!("5 does not fit beside the peer's unused 6")
    };
    assert_eq!((refusals[0].index, refusals[0].kind), (0, RefusalKind::Rate));
    let four = Hold {
        amount: 4,
        ..total.clone()
    };
    let u = uid();
    assert!(admitted(&admit(s, u, &[four.clone(), peer.clone()]).await));
    assert_eq!(
        view(s, &peer).await.remaining,
        6,
        "a headroom hold is never charged"
    );
    let mut used = peer.clone();
    used.mode = HoldMode::Enforce;
    used.amount = 6;
    assert!(admitted(&admit(s, uid(), &[used]).await));
    let six = Hold {
        amount: 6,
        ..total.clone()
    };
    assert!(
        admitted(&admit(s, uid(), &[six, peer.clone()]).await),
        "once the peer used its share there is nothing to keep free"
    );
    settle(s, u, &[four, peer.clone()], &[4, 0]).await.unwrap();
    assert_eq!(
        view(s, &peer).await.remaining,
        0,
        "settlement leaves a headroom hold alone"
    );
}

pub(crate) async fn concurrency_and_attempts<S: CounterStore>(s: &S) {
    let c = conc(id("conc"), 2);
    let a = uid();
    let b = uid();
    assert!(admitted(&admit(s, a, std::slice::from_ref(&c)).await));
    assert!(admitted(&admit(s, b, std::slice::from_ref(&c)).await));
    let AdmitDecision::Refused { refusals } = admit(s, uid(), std::slice::from_ref(&c)).await else {
        panic!()
    };
    assert_eq!(refusals[0].wait_us, Some(crate::math::CONCURRENCY_WAIT_US));
    let node = gcra(id("node"), 1, HOUR_US, 1);
    assert!(admitted(&attempt(s, a, 1, &node).await));
    assert_eq!(
        attempt(s, a, 1, &node).await,
        AdmitDecision::Replayed(ResvState::Held)
    );
    assert!(matches!(
        attempt(s, b, 1, &node).await,
        AdmitDecision::Refused { .. }
    ));
    settle(s, a, &[c.clone(), node.clone()], &[0, 0]).await.unwrap();
    assert_eq!(view(s, &node).await.remaining, 1);
    assert!(admitted(&admit(s, uid(), std::slice::from_ref(&c)).await));
    assert!(settle(s, b, &[c.clone(), node.clone()], &[0, 0]).await.is_err());
    let zero = conc(id("zero"), 0);
    let AdmitDecision::Refused { refusals } = admit(s, uid(), &[zero]).await else {
        panic!()
    };
    assert_eq!(refusals[0].kind, RefusalKind::Exceeds);
    let mut shadow = gcra(id("shadow"), 1, HOUR_US, 1);
    shadow.mode = HoldMode::Shadow;
    for _ in 0..3 {
        assert!(admitted(&admit(s, uid(), std::slice::from_ref(&shadow)).await));
    }
    assert_eq!(view(s, &shadow).await.remaining, 0);
}

/// Many tasks admitting at once never take more than the quota allows.
pub(crate) async fn concurrent_no_overshoot<S: CounterStore + 'static>(s: Arc<S>, tasks: usize) {
    let rpm = gcra(id("rpm"), 10, HOUR_US, 1);
    let usd = budget(id("budget"), 3_500_000_000, 1_000_000_000);
    let tpd = gcra(id("tpd"), 1000, DAY_US, 300);
    let mut set = JoinSet::new();
    for i in 0..tasks {
        let s = Arc::clone(&s);
        let holds = match i % 3 {
            0 => vec![rpm.clone()],
            1 => vec![usd.clone()],
            _ => vec![tpd.clone()],
        };
        set.spawn(async move { (i % 3, admitted(&admit(&*s, uid(), &holds).await)) });
    }
    let mut counts = [0usize; 3];
    while let Some(r) = set.join_next().await {
        let (k, ok) = r.unwrap();
        if ok {
            counts[k] += 1;
        }
    }
    assert_eq!(counts, [10.min(tasks.div_ceil(3)), 3, 3]);
}

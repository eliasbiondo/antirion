//! A node's accounting: the reservation engine over the node's counters, the ledger, and budget
//! counters rebuilt from the ledger the first time a period is used (after a restart, a reset or a
//! changed period). Admissions of counters already rebuilt take no lock shared across counters.

use std::collections::{BTreeSet, HashMap};
use std::hash::{BuildHasher, Hash};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use antirion_core::{Clock, UtcMicros};
use antirion_error::GatewayError;
use parking_lot::Mutex;

use crate::applicable::NamedBudget;
use crate::book::{Book, LedgerHistory, WINDOW};
use crate::counter::{CounterId, Hold, HoldMode, Rule};
use crate::engine::{AttemptRefused, EngineConfig, LimitsEngine, Reservation, SettleReport, Settlement};
use crate::ledger::{BudgetCharge, LedgerLine};
use crate::level::Level;
use crate::math::Window;
use crate::overlay::Overlay;
use crate::period::PeriodBounds;
use crate::plan::{AdmitPlan, PlannedHold};
use crate::store::memory::MemoryCounters;
use crate::store::{CounterStore, CounterView, Counters, NegativeSpend, StoreError};

type Partition = Vec<(String, String)>;

/// How long a node's reservations outlive its last heartbeat in the counter store.
pub const LEASE_TTL: Duration = Duration::from_secs(60);

const SHARDS: usize = 16;

/// A map split over independently locked shards by key.
#[derive(Debug)]
struct Sharded<K, V> {
    shards: [Mutex<HashMap<K, V>>; SHARDS],
    hasher: std::collections::hash_map::RandomState,
}

impl<K: Hash + Eq, V> Default for Sharded<K, V> {
    fn default() -> Sharded<K, V> {
        Sharded {
            shards: std::array::from_fn(|_| Mutex::new(HashMap::new())),
            hasher: std::collections::hash_map::RandomState::new(),
        }
    }
}

impl<K: Hash + Eq, V> Sharded<K, V> {
    fn shard(&self, k: &K) -> &Mutex<HashMap<K, V>> {
        let [first, ..] = &self.shards;
        let i = usize::try_from(self.hasher.hash_one(k) % SHARDS as u64).unwrap_or(0);
        self.shards.get(i).unwrap_or(first)
    }

    fn clear(&self) {
        for s in &self.shards {
            s.lock().clear();
        }
    }
}

/// The rebuild of one budget counter for one period, run once by whoever needs it first.
#[derive(Debug, Clone)]
struct Seeding {
    period_start: i64,
    done: Arc<tokio::sync::OnceCell<()>>,
}

/// The reservation engine, the ledger and the bookkeeping between them.
#[derive(Debug)]
pub struct Accounts {
    engine: LimitsEngine<Counters>,
    book: Book,
    history: OnceLock<Arc<dyn LedgerHistory>>,
    /// The latest period each budget counter was rebuilt for.
    seeded: Sharded<CounterId, Seeding>,
    /// Held shared by rebuilds and exclusively by [`Accounts::clear`].
    clearing: tokio::sync::RwLock<()>,
    /// Budget identities (partition included) already in `partitions`.
    remembered: Sharded<CounterId, ()>,
    /// Partition values of budget counters seen on this node, for state listings. Kept in process
    /// memory only.
    partitions: Mutex<HashMap<(Level, String, String), BTreeSet<Partition>>>,
    overlay: Overlay,
    clock: Arc<dyn Clock>,
}

/// A budget counter's state in its current period.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BudgetState {
    pub period: PeriodBounds,
    pub view: CounterView,
    /// The amount admission refuses at: the declared amount plus the running grants.
    pub effective: u128,
}

impl Accounts {
    /// A standalone node's accounts, on process memory.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, node: &str) -> Accounts {
        let store = Counters::Memory(Box::new(MemoryCounters::new(Arc::clone(&clock))));
        Accounts::with_counters(clock, node, store, Duration::from_millis(100))
    }

    /// Accounts on `store`, whose every call must answer within `store_timeout`.
    #[must_use]
    pub fn with_counters(
        clock: Arc<dyn Clock>,
        node: &str,
        store: Counters,
        store_timeout: Duration,
    ) -> Accounts {
        let cfg = EngineConfig {
            node: node.to_owned(),
            lease_ttl: LEASE_TTL,
            store_timeout,
            replay_capacity: 10_000,
        };
        Accounts {
            clock: Arc::clone(&clock),
            book: Book::new(clock.now(), WINDOW),
            engine: LimitsEngine::new(store, clock, cfg),
            history: OnceLock::new(),
            seeded: Sharded::default(),
            clearing: tokio::sync::RwLock::new(()),
            remembered: Sharded::default(),
            partitions: Mutex::new(HashMap::new()),
            overlay: Overlay::default(),
        }
    }

    /// Grants, last seen spend and request counts of budgets.
    #[must_use]
    pub fn overlay(&self) -> &Overlay {
        &self.overlay
    }

    /// The exact spend charged to a budget by the lines admitted within `period`.
    pub async fn charged(&self, identity: CounterId, period: PeriodBounds) -> Result<i128, StoreError> {
        match (self.book.charged(identity, period), self.history.get()) {
            (Some(s), _) => Ok(s),
            (None, Some(h)) => h.charged(&self.book, identity, period).await,
            (None, None) => Ok(self.book.window_charged(identity, period)),
        }
    }

    /// Where budget rebuilds read the lines the ledger window no longer holds; without it the
    /// window is the whole ledger. Set once.
    pub fn set_history(&self, history: Arc<dyn LedgerHistory>) {
        let _ = self.history.set(history);
    }

    #[must_use]
    pub fn engine(&self) -> &LimitsEngine<Counters> {
        &self.engine
    }

    #[must_use]
    pub fn book(&self) -> &Book {
        &self.book
    }

    /// Sets each budget counter's spend from the ledger the first time its period is used; a
    /// rolling budget's sub-windows the first time its counter is used.
    async fn seed(&self, holds: impl Iterator<Item = (&Hold, CounterId)>) -> Result<(), StoreError> {
        for (hold, identity) in holds {
            let start = match &hold.rule {
                Rule::Budget { period, .. } => period.start.0,
                Rule::Rolling(_) => 0,
                _ => continue,
            };
            let cell = {
                let mut shard = self.seeded.shard(&hold.counter).lock();
                let entry = shard.entry(hold.counter).or_insert_with(|| Seeding {
                    period_start: start,
                    done: Arc::default(),
                });
                if entry.period_start != start {
                    *entry = Seeding {
                        period_start: start,
                        done: Arc::default(),
                    };
                }
                if entry.done.initialized() {
                    continue;
                }
                Arc::clone(&entry.done)
            };
            let _clearing = self.clearing.read().await;
            cell.get_or_try_init(|| self.rebuild(hold, identity)).await?;
        }
        Ok(())
    }

    async fn rebuild(&self, hold: &Hold, identity: CounterId) -> Result<(), StoreError> {
        let period = match &hold.rule {
            Rule::Budget { period, .. } => *period,
            Rule::Rolling(w) => return self.rebuild_rolling(hold, identity, *w).await,
            _ => return Ok(()),
        };
        let spent = u128::try_from(self.charged(identity, period).await?).unwrap_or(0);
        if spent > 0 {
            self.engine.store().seed_budget(hold, spent).await?;
        }
        Ok(())
    }

    async fn rebuild_rolling(&self, hold: &Hold, identity: CounterId, w: Window) -> Result<(), StoreError> {
        let from = UtcMicros(w.live_from(self.clock.now().0));
        let slots = match (self.book.charged_slots(identity, w, from), self.history.get()) {
            (Some(s), _) => s,
            (None, Some(h)) => h.charged_slots(&self.book, identity, w, from).await?,
            (None, None) => self.book.window_charged_slots(identity, w, from),
        };
        let slots: Vec<(i64, u128)> = slots
            .into_iter()
            .filter_map(|(k, v)| u128::try_from(v).ok().filter(|v| *v > 0).map(|v| (k, v)))
            .collect();
        if !slots.is_empty() {
            self.engine.store().seed_window(hold, &slots).await?;
        }
        Ok(())
    }

    fn remember(&self, holds: &[PlannedHold]) {
        for h in holds.iter().filter(|h| h.info.unit.is_budget()) {
            let known = self
                .remembered
                .shard(&h.info.identity)
                .lock()
                .insert(h.info.identity, ())
                .is_some();
            if known {
                continue;
            }
            self.partitions
                .lock()
                .entry((h.info.level, h.info.owner.clone(), h.info.name.clone()))
                .or_default()
                .insert(h.info.partition.clone());
        }
    }

    /// Admission: every hold of the plan, or none.
    pub async fn admit(&self, plan: AdmitPlan) -> Result<Reservation, GatewayError> {
        let (admitted, budgets) = self.admit_unnoted(plan).await;
        if let Err(e) = &admitted {
            self.note_refusal(e, &budgets);
        }
        admitted
    }

    /// Admission without counting a refusal against its budget; the plan's budget holds come back
    /// for [`Accounts::note_refusal`].
    pub(crate) async fn admit_unnoted(
        &self,
        mut plan: AdmitPlan,
    ) -> (Result<Reservation, GatewayError>, Vec<PlannedHold>) {
        let holds = plan.holds.iter().map(|h| (&h.hold, h.info.identity));
        if self.seed(holds).await.is_err() {
            return (Err(crate::refusal::unavailable_error()), Vec::new());
        }
        self.remember(&plan.holds);
        self.overlay.raise(&mut plan.holds, plan.admitted_at);
        let budgets: Vec<PlannedHold> = plan
            .holds
            .iter()
            .filter(|h| h.info.unit.is_budget())
            .cloned()
            .collect();
        (self.engine.admit(plan).await, budgets)
    }

    /// Counts the request a budget refused against that budget.
    pub(crate) fn note_refusal(&self, e: &GatewayError, budgets: &[PlannedHold]) {
        if e.code != antirion_error::Code::BudgetExceeded {
            return;
        }
        let Some(limit) = e.limit.as_deref() else {
            return;
        };
        let field = |k: &str| limit.get(k).and_then(serde_json::Value::as_str);
        let refusing = budgets.iter().find(|h| {
            Some(h.info.level.as_str()) == field("level") && Some(h.info.name.as_str()) == field("name")
        });
        if let Some(h) = refusing
            && let Some(period) = h.hold.rule.budget_period(self.clock.now())
        {
            self.overlay.refused(&h.info, period);
        }
    }

    /// Per-attempt holds added to a reservation, all or nothing.
    pub async fn admit_attempt(
        &self,
        r: &mut Reservation,
        attempt: u16,
        holds: Vec<PlannedHold>,
    ) -> Result<(), AttemptRefused> {
        let mut holds = holds;
        let _ = self.seed(holds.iter().map(|h| (&h.hold, h.info.identity))).await;
        self.remember(&holds);
        self.overlay.raise(&mut holds, r.admitted_at);
        self.engine.admit_attempt(r, attempt, holds).await
    }

    /// Settlement of a reservation.
    pub async fn settle(&self, r: &Reservation, s: &Settlement) -> SettleReport {
        let report = self.engine.settle(r, s).await;
        for b in &report.spent {
            self.overlay.saw(b.identity, b.period_start, b.spent);
        }
        report
    }

    /// Adds a shadow call's charges to its budget counters, whatever they hold: nothing was
    /// reserved and nothing is refused. Returns the charges the call's ledger line attributes. The
    /// line must be recorded after this, or a counter seeded from the ledger would count it twice.
    pub async fn charge_shadow(&self, holds: &[PlannedHold]) -> Vec<BudgetCharge> {
        let _ = self.seed(holds.iter().map(|h| (&h.hold, h.info.identity))).await;
        self.remember(holds);
        let mut out = Vec::with_capacity(holds.len());
        for h in holds {
            let amount = i128::try_from(h.hold.amount).unwrap_or(i128::MAX);
            if let Ok(Ok(_)) = self.engine.store().adjust_budget(&h.hold, amount).await {
                out.push(BudgetCharge {
                    level: h.info.level.as_str().to_owned(),
                    owner: h.info.owner.clone(),
                    name: h.info.name.clone(),
                    identity: h.info.identity,
                    amount,
                });
            }
        }
        out
    }

    /// Appends ledger lines to the window; returns the ones not recorded before.
    pub fn record(&self, lines: Vec<LedgerLine>) -> Vec<Arc<LedgerLine>> {
        self.book
            .append(lines.into_iter().map(|l| (l, ())).collect(), false, |_, _| {})
    }

    fn hold_of(&self, b: &NamedBudget<'_>, now: UtcMicros) -> Hold {
        Hold {
            counter: b.counter,
            group: 0,
            rule: b
                .budget
                .rule(b.quota.saturating_add(self.overlay.extra(b.identity, now)), now),
            amount: 0,
            mode: if b.budget.hard {
                HoldMode::Enforce
            } else {
                HoldMode::Track
            },
        }
    }

    /// A budget counter's state in the period containing `now`.
    pub async fn budget_state(&self, b: &NamedBudget<'_>, now: UtcMicros) -> Result<BudgetState, StoreError> {
        let hold = self.hold_of(b, now);
        self.seed(std::iter::once((&hold, b.identity))).await?;
        let view = self
            .engine
            .store()
            .read(std::slice::from_ref(&hold))
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
        let (period, effective) = match hold.rule {
            Rule::Budget { period, amount } => (period, amount),
            Rule::Rolling(w) => (b.budget.bounds(now), u128::from(w.limit)),
            _ => (b.budget.period.bounds(now), b.quota),
        };
        self.overlay.saw(b.identity, period.start, view.spent);
        Ok(BudgetState {
            period,
            view,
            effective,
        })
    }

    /// The state of each counter of `holds` (holds that reserve nothing, from
    /// [`crate::plan::reading`]), read without reserving: the self-service view.
    pub async fn read(&self, holds: &[PlannedHold]) -> Result<Vec<CounterView>, StoreError> {
        self.seed(holds.iter().map(|h| (&h.hold, h.info.identity)))
            .await?;
        let mut holds = holds.to_vec();
        self.overlay.raise(&mut holds, self.clock.now());
        let raw: Vec<Hold> = holds.iter().map(|h| h.hold.clone()).collect();
        self.engine.store().read(&raw).await
    }

    /// Adds `delta` to a budget's settled spend in the current period (negative: a credit).
    pub async fn adjust(
        &self,
        b: &NamedBudget<'_>,
        now: UtcMicros,
        delta: i128,
    ) -> Result<Result<u128, NegativeSpend>, StoreError> {
        let hold = self.hold_of(b, now);
        self.seed(std::iter::once((&hold, b.identity))).await?;
        self.engine.store().adjust_budget(&hold, delta).await
    }

    /// The partition values of a budget's counters seen on this node.
    #[must_use]
    pub fn partitions_of(&self, level: Level, owner: &str, name: &str) -> Vec<Partition> {
        self.partitions
            .lock()
            .get(&(level, owner.to_owned(), name.to_owned()))
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Drops closed reservations past their idempotency window and idle counters.
    pub fn sweep(&self) {
        self.engine.store().sweep();
    }

    /// Forgets every counter, reservation and ledger line (test-mode reset).
    pub async fn clear(&self) {
        let _clearing = self.clearing.write().await;
        self.engine.store().clear();
        self.book.clear();
        self.seeded.clear();
        self.remembered.clear();
        self.partitions.lock().clear();
        self.overlay.clear();
    }
}

#[cfg(test)]
mod tests {
    use antirion_core::ResourceUid;
    use antirion_testkit::FakeClock;
    use serde_json::json;

    use super::*;
    use crate::applicable::{RequestAttrs, named_budget};
    use crate::cost::{CostStatus, FixedUnits};
    use crate::estimate::{Estimate, OutSource};
    use crate::ledger::{BudgetCharge, LineStatus, Purpose, Reserved, UsageSource};
    use crate::money::{Dec, Nusd};
    use crate::plan::{AdmitInput, plan};
    use crate::policy::Policies;
    use crate::price::Price;
    use crate::store::ReservationId;
    use crate::usage::Usage;
    use crate::weights::Incurred;

    fn policies(period: &str) -> Policies {
        let doc = json!({"keys": {"k": {"policies": {"budgets": {
            "b": {"unit": "usd", "amount": "1.00", "period": period}
        }}}}});
        Policies::compile(&doc, &|_, n| ResourceUid(n.len() as u128 + 1)).unwrap()
    }

    fn line(id: &str, at: UtcMicros, identity: CounterId, nusd: i128) -> LedgerLine {
        LedgerLine {
            request_id: id.into(),
            line: 0,
            purpose: Purpose::Primary,
            shadow: false,
            status: LineStatus::Success,
            admitted_at: at,
            settled_at: at,
            organization: "default".into(),
            team: "default".into(),
            project: "default".into(),
            key: "k".into(),
            user: None,
            end_user: None,
            tags: vec![],
            model: None,
            route: None,
            provider: None,
            deployment: None,
            upstream_model: None,
            call: crate::ledger::CallTarget::default(),
            endpoint: "chat_completions".into(),
            stream: false,
            usage: Usage::default(),
            usage_raw: None,
            usage_source: UsageSource::Upstream,
            cost: Some(Nusd(nusd)),
            billed: Some(Nusd(nusd)),
            cost_status: CostStatus::Exact,
            price_version: None,
            service_tier: None,
            avoided: None,
            reserved: Reserved {
                input: 0,
                output: 0,
                usd: None,
                out_source: OutSource::Request,
            },
            budgets: vec![BudgetCharge {
                level: "key".into(),
                owner: "k".into(),
                name: "b".into(),
                identity,
                amount: nusd,
            }],
        }
    }

    #[tokio::test]
    async fn a_new_period_is_rebuilt_from_the_lines_admitted_within_it() {
        let day = 86_400_000_000;
        let clock = Arc::new(FakeClock::at(UtcMicros(100 * day + 5)));
        let accounts = Accounts::new(clock.clone(), "n");
        let lifetime = policies("lifetime");
        let b = named_budget(&lifetime, Level::Key, "k", "b", &[], None).unwrap();
        accounts.record(vec![
            line("r1", UtcMicros(98 * day), b.identity, 10_000_000),
            line("r2", UtcMicros(100 * day + 1), b.identity, 20_000_000),
        ]);
        let st = accounts.budget_state(&b, clock.now()).await.unwrap();
        assert_eq!(st.view.spent, 30_000_000);
        let daily = policies("daily");
        let d = named_budget(&daily, Level::Key, "k", "b", &[], None).unwrap();
        assert_eq!(d.identity, b.identity);
        assert_ne!(d.counter, b.counter);
        let st = accounts.budget_state(&d, clock.now()).await.unwrap();
        assert_eq!(st.view.spent, 20_000_000);
        assert_eq!(st.period.start, UtcMicros(100 * day));
        assert!(
            accounts
                .adjust(&d, clock.now(), -30_000_000)
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(
            accounts.adjust(&d, clock.now(), -5_000_000).await.unwrap(),
            Ok(15_000_000)
        );
        accounts.clear().await;
        assert_eq!(
            accounts.budget_state(&d, clock.now()).await.unwrap().view.spent,
            0
        );
    }

    fn admit_input<'a>(
        uid: u128,
        out: u64,
        cands: &'a [Option<&'a Price>],
        now: UtcMicros,
    ) -> AdmitInput<'a> {
        AdmitInput {
            uid: ReservationId(uid),
            attrs: RequestAttrs {
                organization: "default",
                team: "default",
                project: "default",
                key: "k",
                model: Some("m"),
                endpoint: "chat_completions",
                ..RequestAttrs::default()
            },
            estimate: Estimate {
                input: 1,
                output: out,
                out_source: OutSource::Request,
                clamp_to: None,
            },
            hedge: 1,
            incurs: Incurred::default(),
            candidates: cands,
            fixed: FixedUnits::default(),
            multiplier: Dec::ONE,
            now,
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(24))]

        /// Concurrent admissions and settlements never take a hard budget past its amount when
        /// every request consumes at most what it reserved, and the counter equals the ledger.
        #[test]
        fn concurrent_spend_stays_within_the_budget_and_equals_the_ledger(
            reqs in proptest::collection::vec((1u64..150, 0u64..=100), 1..60),
        ) {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_time()
                .build()
                .unwrap();
            let (spent, reserved, ledger) = runtime.block_on(async {
                let clock = Arc::new(FakeClock::at(UtcMicros(1_000)));
                let accounts = Arc::new(Accounts::new(clock.clone(), "n"));
                let p = Arc::new(policies("lifetime"));
                let price = Price::parse(&json!({"input": "0", "output": "10000.00"})).unwrap();
                let mut tasks = tokio::task::JoinSet::new();
                for (i, (out, pct)) in reqs.into_iter().enumerate() {
                    let (accounts, p, price, now) = (Arc::clone(&accounts), Arc::clone(&p), price.clone(), clock.now());
                    tasks.spawn(async move {
                        let cands = [Some(&price)];
                        let planned = plan(&p, &admit_input(i as u128 + 1, out, &cands, now)).unwrap();
                        let Ok(r) = accounts.admit(planned).await else { return };
                        let actual = i128::from(out * pct / 100) * 10_000_000;
                        let b = named_budget(&p, Level::Key, "k", "b", &[], None).unwrap();
                        let mut l = line(&format!("r{i}"), now, b.identity, actual);
                        l.budgets.retain(|c| c.amount != 0);
                        accounts.record(vec![l]);
                        let s = Settlement { usage: Usage::default(), billed: Nusd(actual), success: true };
                        accounts.settle(&r, &s).await;
                    });
                }
                while tasks.join_next().await.is_some() {}
                let b = named_budget(&p, Level::Key, "k", "b", &[], None).unwrap();
                let st = accounts.budget_state(&b, clock.now()).await.unwrap();
                let all = PeriodBounds { start: UtcMicros::EPOCH, end: None };
                (st.view.spent, st.view.reserved, accounts.book().window_charged(b.identity, all))
            });
            proptest::prop_assert!(spent <= 1_000_000_000);
            proptest::prop_assert_eq!(reserved, 0);
            proptest::prop_assert_eq!(i128::try_from(spent).unwrap(), ledger);
        }
    }

    #[tokio::test]
    async fn admissions_see_the_rebuilt_spend() {
        let clock = Arc::new(FakeClock::at(UtcMicros(1_000)));
        let accounts = Accounts::new(clock.clone(), "n");
        let p = policies("lifetime");
        let b = named_budget(&p, Level::Key, "k", "b", &[], None).unwrap();
        accounts.record(vec![line("r1", UtcMicros(10), b.identity, 950_000_000)]);
        let price = Price::parse(&json!({"input": "0", "output": "10000.00"})).unwrap();
        let cands = [Some(&price)];
        let input = |uid: u128, out: u64| AdmitInput {
            uid: ReservationId(uid),
            attrs: RequestAttrs {
                organization: "default",
                team: "default",
                project: "default",
                key: "k",
                model: Some("m"),
                endpoint: "chat_completions",
                ..RequestAttrs::default()
            },
            estimate: Estimate {
                input: 1,
                output: out,
                out_source: OutSource::Request,
                clamp_to: None,
            },
            hedge: 1,
            incurs: Incurred::default(),
            candidates: &cands,
            fixed: FixedUnits::default(),
            multiplier: Dec::ONE,
            now: clock.now(),
        };
        assert!(accounts.admit(plan(&p, &input(1, 10)).unwrap()).await.is_err());
        let r = accounts.admit(plan(&p, &input(2, 5)).unwrap()).await.unwrap();
        let report = accounts
            .settle(
                &r,
                &Settlement {
                    usage: Usage::default(),
                    billed: Nusd(10_000_000),
                    success: true,
                },
            )
            .await;
        assert!(!report.queued);
        let st = accounts.budget_state(&b, clock.now()).await.unwrap();
        assert_eq!((st.view.spent, st.view.reserved), (960_000_000, 0));
        assert_eq!(accounts.partitions_of(Level::Key, "k", "b"), vec![Vec::new()]);
    }
}

//! The spend ledger as a node holds it in memory: a bounded window of the most recent lines,
//! idempotent by `(request_id, line)` within it and indexed for the queries of the Admin API (by
//! request, and each budget's charges by admission time). Every line also goes to the store, which
//! answers for the lines the window no longer holds.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use antirion_core::UtcMicros;
use parking_lot::RwLock;
use serde_json::{Map, Value, json};

use crate::counter::CounterId;
use crate::ledger::LedgerLine;
use crate::money::Nusd;
use crate::period::PeriodBounds;
use crate::store::StoreError;

/// The lines the window keeps once the store holds them.
pub const WINDOW: usize = 65_536;

/// A line in the window. `seq` orders appends; a line read from the store is `stored` from the
/// start, the node's own lines once the writer reported them durable.
#[derive(Debug)]
struct Slot {
    line: Arc<LedgerLine>,
    seq: u64,
    stored: bool,
}

#[derive(Copy, Clone, Debug)]
struct Charge {
    seq: u64,
    at: i64,
    amount: i128,
    stored: bool,
}

#[derive(Debug)]
struct Window {
    slots: VecDeque<Slot>,
    next_seq: u64,
    /// Every line appended with a smaller `seq` is in the store.
    durable_below: u64,
    by_request: HashMap<String, Vec<(u32, u64)>>,
    charges: HashMap<CounterId, VecDeque<Charge>>,
    /// Every line this node appended that was admitted at or after this instant is in the window.
    covers_from: i64,
}

impl Window {
    fn new(covers_from: i64, next_seq: u64) -> Window {
        Window {
            slots: VecDeque::new(),
            next_seq,
            durable_below: next_seq,
            by_request: HashMap::new(),
            charges: HashMap::new(),
            covers_from,
        }
    }

    fn durable(&self, seq: u64, stored: bool) -> bool {
        stored || seq < self.durable_below
    }

    /// Drops the oldest line, keeping the indexes and the coverage in step.
    fn evict(&mut self) {
        let Some(slot) = self.slots.pop_front() else {
            return;
        };
        let l = &slot.line;
        if let Some(lines) = self.by_request.get_mut(&l.request_id) {
            lines.retain(|(_, seq)| *seq != slot.seq);
            if lines.is_empty() {
                self.by_request.remove(&l.request_id);
            }
        }
        for b in &l.budgets {
            if let Some(q) = self.charges.get_mut(&b.identity) {
                if q.front().is_some_and(|c| c.seq == slot.seq) {
                    q.pop_front();
                } else {
                    q.retain(|c| c.seq != slot.seq);
                }
                if q.is_empty() {
                    self.charges.remove(&b.identity);
                }
            }
        }
        self.covers_from = self.covers_from.max(l.admitted_at.0.saturating_add(1));
    }

    /// Evicts down to `window` lines, never a line the store does not hold yet unless the window
    /// is four times over.
    fn trim(&mut self, window: usize) {
        while let Some(front) = self.slots.front() {
            let over = self.slots.len() > window;
            let far_over = self.slots.len() > window.saturating_mul(4);
            if !(far_over || (over && self.durable(front.seq, front.stored))) {
                return;
            }
            self.evict();
        }
    }
}

/// The ledger window of a node.
#[derive(Debug)]
pub struct Book {
    inner: RwLock<Window>,
    window: usize,
}

impl Default for Book {
    /// An empty ledger with nothing before it: the window answers for every period.
    fn default() -> Book {
        Book::new(UtcMicros::EPOCH, WINDOW)
    }
}

/// The filters of a ledger query; absent fields match every line.
#[derive(Clone, Debug, Default)]
pub struct LedgerFilter {
    /// Lines admitted at or after `from` and before `to`.
    pub from: Option<UtcMicros>,
    pub to: Option<UtcMicros>,
    pub organization: Option<String>,
    pub team: Option<String>,
    pub project: Option<String>,
    pub key: Option<String>,
    pub user: Option<String>,
    pub end_user: Option<String>,
    pub model: Option<String>,
    pub deployment: Option<String>,
    pub tag: Option<String>,
    pub status: Option<String>,
}

fn same(want: Option<&String>, have: Option<&str>) -> bool {
    want.is_none_or(|w| have == Some(w.as_str()))
}

impl LedgerFilter {
    #[must_use]
    pub fn accepts(&self, l: &LedgerLine) -> bool {
        self.from.is_none_or(|f| l.admitted_at >= f)
            && self.to.is_none_or(|t| l.admitted_at < t)
            && same(self.organization.as_ref(), Some(&l.organization))
            && same(self.team.as_ref(), Some(&l.team))
            && same(self.project.as_ref(), Some(&l.project))
            && same(self.key.as_ref(), Some(&l.key))
            && same(self.user.as_ref(), l.user.as_deref())
            && same(self.end_user.as_ref(), l.end_user.as_deref())
            && same(self.model.as_ref(), l.model.as_deref())
            && same(self.deployment.as_ref(), l.deployment.as_deref())
            && same(self.status.as_ref(), Some(l.status.as_str()))
            && self.tag.as_ref().is_none_or(|t| l.tags.contains(t))
    }
}

fn within(at: i64, period: PeriodBounds) -> bool {
    at >= period.start.0 && period.end.is_none_or(|e| at < e.0)
}

impl Book {
    /// A window keeping `window` lines, holding every line admitted from `covers_from` on (the
    /// node's start: older lines are only in the store).
    #[must_use]
    pub fn new(covers_from: UtcMicros, window: usize) -> Book {
        Book {
            inner: RwLock::new(Window::new(covers_from.0, 0)),
            window: window.max(1),
        }
    }

    /// Appends the lines not in the window; returns the ones appended. `stored`: the lines were
    /// read from the store. `sink` receives, under the window's lock, the payloads of the appended
    /// lines and the sequence number below which every line has been appended, so a writer told
    /// later that everything it took is durable can say so with [`Book::mark_durable`].
    pub fn append<T>(
        &self,
        lines: Vec<(LedgerLine, T)>,
        stored: bool,
        sink: impl FnOnce(Vec<T>, u64),
    ) -> Vec<Arc<LedgerLine>> {
        let mut w = self.inner.write();
        let mut added = Vec::with_capacity(lines.len());
        let mut payloads = Vec::with_capacity(lines.len());
        for (line, payload) in lines {
            let seen = w
                .by_request
                .get(&line.request_id)
                .is_some_and(|ls| ls.iter().any(|(n, _)| *n == line.line));
            if seen {
                continue;
            }
            let seq = w.next_seq;
            w.next_seq += 1;
            for b in &line.budgets {
                w.charges.entry(b.identity).or_default().push_back(Charge {
                    seq,
                    at: line.admitted_at.0,
                    amount: b.amount,
                    stored,
                });
            }
            w.by_request
                .entry(line.request_id.clone())
                .or_default()
                .push((line.line, seq));
            let line = Arc::new(line);
            w.slots.push_back(Slot {
                line: Arc::clone(&line),
                seq,
                stored,
            });
            added.push(line);
            payloads.push(payload);
        }
        let below = w.next_seq;
        sink(payloads, below);
        w.trim(self.window);
        added
    }

    /// Every line appended with a sequence number below `below` is in the store.
    pub fn mark_durable(&self, below: u64) {
        let mut w = self.inner.write();
        w.durable_below = w.durable_below.max(below.min(w.next_seq));
        w.trim(self.window);
    }

    /// The window's lines of one request, by line number.
    #[must_use]
    pub fn request(&self, request_id: &str) -> Vec<Arc<LedgerLine>> {
        let w = self.inner.read();
        let mut out: Vec<Arc<LedgerLine>> = w
            .by_request
            .get(request_id)
            .into_iter()
            .flatten()
            .filter_map(|(_, seq)| {
                let ix = usize::try_from(seq.checked_sub(w.slots.front()?.seq)?).ok()?;
                w.slots.get(ix).map(|s| Arc::clone(&s.line))
            })
            .collect();
        out.sort_by_key(|l| l.line);
        out
    }

    /// The window's lines a filter accepts, in append order.
    #[must_use]
    pub fn select(&self, f: &LedgerFilter) -> Vec<Arc<LedgerLine>> {
        self.inner
            .read()
            .slots
            .iter()
            .filter(|s| f.accepts(&s.line))
            .map(|s| Arc::clone(&s.line))
            .collect()
    }

    /// Whether the window holds every line this node appended that was admitted at or after
    /// `from`.
    #[must_use]
    pub fn covers(&self, from: UtcMicros) -> bool {
        from.0 >= self.inner.read().covers_from
    }

    /// The spend charged to a budget by the window's lines admitted within `period`; `None` when
    /// the period starts before what the window covers.
    #[must_use]
    pub fn charged(&self, identity: CounterId, period: PeriodBounds) -> Option<i128> {
        let w = self.inner.read();
        (period.start.0 >= w.covers_from).then(|| sum(&w, identity, period, |_| true))
    }

    /// [`Book::charged`] over whatever the window holds, covered or not.
    #[must_use]
    pub fn window_charged(&self, identity: CounterId, period: PeriodBounds) -> i128 {
        sum(&self.inner.read(), identity, period, |_| true)
    }

    /// The spend charged within `period` by the window's lines the store does not hold yet.
    #[must_use]
    pub fn unstored_charged(&self, identity: CounterId, period: PeriodBounds) -> i128 {
        let w = self.inner.read();
        sum(&w, identity, period, |c| !w.durable(c.seq, c.stored))
    }

    /// The spend charged to a rolling budget by the window's lines admitted from `from`, by
    /// sub-window of `rolling`; `None` when `from` is before what the window covers.
    #[must_use]
    pub fn charged_slots(
        &self,
        identity: CounterId,
        rolling: crate::math::Window,
        from: UtcMicros,
    ) -> Option<Slots> {
        let w = self.inner.read();
        (from.0 >= w.covers_from).then(|| slot_sums(&w, identity, rolling, from, |_| true))
    }

    /// [`Book::charged_slots`] over whatever the window holds, covered or not.
    #[must_use]
    pub fn window_charged_slots(
        &self,
        identity: CounterId,
        rolling: crate::math::Window,
        from: UtcMicros,
    ) -> Slots {
        slot_sums(&self.inner.read(), identity, rolling, from, |_| true)
    }

    /// [`Book::charged_slots`] of the window's lines the store does not hold yet.
    #[must_use]
    pub fn unstored_charged_slots(
        &self,
        identity: CounterId,
        rolling: crate::math::Window,
        from: UtcMicros,
    ) -> Slots {
        let w = self.inner.read();
        slot_sums(&w, identity, rolling, from, |c| !w.durable(c.seq, c.stored))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().slots.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Forgets every line: the ledger starts again empty, in the store as here.
    pub fn clear(&self) {
        let mut w = self.inner.write();
        *w = Window::new(UtcMicros::EPOCH.0, w.next_seq);
    }
}

/// Charges by sub-window index of a rolling budget.
pub type Slots = BTreeMap<i64, i128>;

fn slot_sums(
    w: &Window,
    identity: CounterId,
    rolling: crate::math::Window,
    from: UtcMicros,
    keep: impl Fn(&Charge) -> bool,
) -> Slots {
    let mut out = Slots::new();
    for c in w.charges.get(&identity).into_iter().flatten() {
        if c.at >= from.0 && keep(c) {
            let slot = out.entry(rolling.slot(c.at)).or_insert(0);
            *slot = slot.saturating_add(c.amount);
        }
    }
    out
}

fn sum(w: &Window, identity: CounterId, period: PeriodBounds, keep: impl Fn(&Charge) -> bool) -> i128 {
    w.charges
        .get(&identity)
        .into_iter()
        .flatten()
        .filter(|c| within(c.at, period) && keep(c))
        .fold(0i128, |acc, c| acc.saturating_add(c.amount))
}

/// Where a node reads what its window no longer holds.
pub trait LedgerHistory: Send + Sync + std::fmt::Debug {
    /// The exact spend charged to a budget by every line admitted within `period`, the window's
    /// lines the store does not hold yet included.
    fn charged<'a>(
        &'a self,
        book: &'a Book,
        identity: CounterId,
        period: PeriodBounds,
    ) -> Pin<Box<dyn Future<Output = Result<i128, StoreError>> + Send + 'a>>;

    /// The spend charged to a rolling budget by every line admitted from `from`, by sub-window
    /// of `rolling`, the window's lines the store does not hold yet included.
    fn charged_slots<'a>(
        &'a self,
        book: &'a Book,
        identity: CounterId,
        rolling: crate::math::Window,
        from: UtcMicros,
    ) -> Pin<Box<dyn Future<Output = Result<Slots, StoreError>> + Send + 'a>>;
}

/// The attributes a spend summary groups by.
pub const GROUP_ATTRS: &[&str] = &[
    "organization",
    "team",
    "project",
    "key",
    "user",
    "end_user",
    "model",
    "route",
    "provider",
    "deployment",
    "upstream_model",
    "endpoint",
    "status",
    "purpose",
    "tag",
];

fn attr_values(l: &LedgerLine, attr: &str) -> Vec<Value> {
    let opt = |v: &Option<String>| vec![v.clone().map_or(Value::Null, Value::String)];
    match attr {
        "organization" => vec![Value::String(l.organization.clone())],
        "team" => vec![Value::String(l.team.clone())],
        "project" => vec![Value::String(l.project.clone())],
        "key" => vec![Value::String(l.key.clone())],
        "user" => opt(&l.user),
        "end_user" => opt(&l.end_user),
        "model" => opt(&l.model),
        "route" => opt(&l.route),
        "provider" => opt(&l.provider),
        "deployment" => opt(&l.deployment),
        "upstream_model" => opt(&l.upstream_model),
        "endpoint" => vec![Value::String(l.endpoint.clone())],
        "status" => vec![Value::from(l.status.as_str())],
        "purpose" => vec![Value::from(l.purpose.as_str())],
        "tag" if l.tags.is_empty() => vec![Value::Null],
        "tag" => l.tags.iter().cloned().map(Value::String).collect(),
        _ => vec![Value::Null],
    }
}

#[derive(Default)]
struct Row {
    requests: HashSet<String>,
    lines: u64,
    tokens: BTreeMap<&'static str, u64>,
    billed: i128,
    cost: i128,
    saved: i128,
}

/// One summary row per distinct value tuple of `group_by`, ordered by group; amounts are exact sums
/// of the lines.
#[must_use]
pub fn summary(lines: &[Arc<LedgerLine>], group_by: &[String]) -> Vec<Value> {
    let mut rows: BTreeMap<String, (Map<String, Value>, Row)> = BTreeMap::new();
    for l in lines {
        let mut groups: Vec<Map<String, Value>> = vec![Map::new()];
        for attr in group_by {
            let values = attr_values(l, attr);
            groups = groups
                .into_iter()
                .flat_map(|g| {
                    values.iter().map(move |v| {
                        let mut g = g.clone();
                        g.insert(attr.clone(), v.clone());
                        g
                    })
                })
                .collect();
        }
        for g in groups {
            let key = Value::Object(g.clone()).to_string();
            let (_, row) = rows.entry(key).or_insert_with(|| (g, Row::default()));
            row.requests.insert(l.request_id.clone());
            row.lines += 1;
            for (name, n) in l.usage.fields() {
                let t = row.tokens.entry(name).or_insert(0);
                *t = t.saturating_add(n);
            }
            row.billed = row.billed.saturating_add(l.billed.map_or(0, |n| n.0));
            row.cost = row.cost.saturating_add(l.cost.map_or(0, |n| n.0));
            row.saved = row.saved.saturating_add(l.avoided.map_or(0, |n| n.0));
        }
    }
    rows.into_values()
        .map(|(group, row)| {
            json!({
                "group": group,
                "requests": row.requests.len(),
                "lines": row.lines,
                "tokens": row.tokens,
                "cost": {
                    "billed_usd": Nusd(row.billed).to_usd_string(),
                    "cost_usd": Nusd(row.cost).to_usd_string(),
                },
                "cache_saved_usd": Nusd(row.saved).to_usd_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::CostStatus;
    use crate::estimate::OutSource;
    use crate::ledger::{BudgetCharge, LineStatus, Purpose, Reserved, UsageSource};
    use crate::usage::Usage;

    fn line(
        request: &str,
        n: u32,
        key: &str,
        at: i64,
        billed: i128,
        budget: Option<CounterId>,
    ) -> LedgerLine {
        LedgerLine {
            request_id: request.into(),
            line: n,
            purpose: Purpose::Primary,
            shadow: false,
            status: LineStatus::Success,
            admitted_at: UtcMicros(at),
            settled_at: UtcMicros(at),
            organization: "o".into(),
            team: "t".into(),
            project: "p".into(),
            key: key.into(),
            user: None,
            end_user: None,
            tags: vec!["a".into(), "b".into()],
            model: Some("m".into()),
            route: None,
            provider: None,
            deployment: Some("d".into()),
            upstream_model: None,
            call: crate::ledger::CallTarget::default(),
            endpoint: "chat_completions".into(),
            stream: false,
            usage: Usage {
                input: 10,
                output: 5,
                ..Usage::default()
            },
            usage_raw: None,
            usage_source: UsageSource::Upstream,
            cost: Some(Nusd(billed)),
            billed: Some(Nusd(billed)),
            cost_status: CostStatus::Exact,
            price_version: None,
            service_tier: None,
            avoided: None,
            reserved: Reserved {
                input: 1,
                output: 1,
                usd: None,
                out_source: OutSource::Request,
            },
            budgets: budget
                .map(|identity| BudgetCharge {
                    level: "key".into(),
                    owner: key.into(),
                    name: "b".into(),
                    identity,
                    amount: billed,
                })
                .into_iter()
                .collect(),
        }
    }

    fn add(book: &Book, lines: Vec<LedgerLine>) -> usize {
        book.append(lines.into_iter().map(|l| (l, ())).collect(), false, |_, _| {})
            .len()
    }

    const ALL: PeriodBounds = PeriodBounds {
        start: UtcMicros::EPOCH,
        end: None,
    };

    #[test]
    fn appends_are_idempotent_and_budget_sums_follow_periods() {
        let book = Book::default();
        let id = CounterId([7; 16]);
        assert_eq!(add(&book, vec![line("r1", 0, "k", 10, 100, Some(id))]), 1);
        assert_eq!(add(&book, vec![line("r1", 0, "k", 10, 999, Some(id))]), 0);
        add(
            &book,
            vec![
                line("r2", 0, "k", 20, 50, Some(id)),
                line("r2", 1, "k", 20, 0, None),
            ],
        );
        assert_eq!(book.charged(id, ALL), Some(150));
        let late = PeriodBounds {
            start: UtcMicros(15),
            end: Some(UtcMicros(30)),
        };
        assert_eq!(book.charged(id, late), Some(50));
        assert_eq!(book.request("r2").len(), 2);
        assert_eq!(book.len(), 3);
        book.clear();
        assert!(book.is_empty());
    }

    #[test]
    fn the_window_is_bounded_and_keeps_what_the_store_lacks() {
        let book = Book::new(UtcMicros(5), 2);
        let id = CounterId([1; 16]);
        let mut through = 0;
        for (i, at) in [10, 20, 30, 40].into_iter().enumerate() {
            book.append(
                vec![(line(&format!("r{i}"), 0, "k", at, 1, Some(id)), ())],
                false,
                |_, below| through = below,
            );
        }
        assert_eq!(book.len(), 4, "nothing is durable yet");
        assert_eq!(book.unstored_charged(id, ALL), 4);
        assert_eq!(book.charged(id, ALL), None, "the window starts after the epoch");
        book.mark_durable(3);
        assert_eq!(book.len(), 2, "durable lines leave the window");
        assert_eq!(book.unstored_charged(id, ALL), 1);
        assert!(book.request("r0").is_empty());
        assert_eq!(book.request("r3").len(), 1);
        assert!(!book.covers(UtcMicros(20)));
        assert!(book.covers(UtcMicros(21)));
        let from_21 = PeriodBounds {
            start: UtcMicros(21),
            end: None,
        };
        assert_eq!(book.charged(id, from_21), Some(2));
        book.mark_durable(through);
        assert_eq!(book.unstored_charged(id, ALL), 0);
        for i in 0..20 {
            book.append(
                vec![(line(&format!("x{i}"), 0, "k", 50, 1, None), ())],
                false,
                |_, _| {},
            );
        }
        assert_eq!(
            book.len(),
            8,
            "four times the window while the store holds nothing"
        );
        assert!(!book.covers(UtcMicros(50)));
    }

    #[test]
    fn stored_lines_are_durable_and_duplicates_are_ignored() {
        let book = Book::new(UtcMicros::EPOCH, 1);
        let lines = |n: u32| (0..n).map(|i| (line("r", i, "k", 1, 1, None), ())).collect();
        assert_eq!(book.append(lines(3), true, |_, _| {}).len(), 3);
        assert_eq!(book.len(), 1);
        assert_eq!(book.request("r").len(), 1);
        assert_eq!(book.append(lines(3), true, |_, _| {}).len(), 2);
    }

    #[test]
    fn filters_and_summaries_are_exact() {
        let book = Book::default();
        add(
            &book,
            vec![
                line("r1", 0, "k1", 10, 3, None),
                line("r2", 0, "k1", 20, 4, None),
                line("r3", 0, "k2", 30, 5, None),
            ],
        );
        let f = LedgerFilter {
            key: Some("k1".into()),
            ..LedgerFilter::default()
        };
        assert_eq!(book.select(&f).len(), 2);
        let f = LedgerFilter {
            from: Some(UtcMicros(20)),
            ..LedgerFilter::default()
        };
        assert_eq!(book.select(&f).len(), 2);
        let rows = summary(&book.select(&LedgerFilter::default()), &["key".to_owned()]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["group"]["key"], "k1");
        assert_eq!(rows[0]["requests"], 2);
        assert_eq!(rows[0]["cost"]["billed_usd"], "0.000000007");
        assert_eq!(rows[0]["tokens"]["input"], 20);
        let by_tag = summary(&book.select(&LedgerFilter::default()), &["tag".to_owned()]);
        assert_eq!(by_tag.len(), 2);
        assert_eq!(by_tag[0]["lines"], 3);
    }
}

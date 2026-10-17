//! Aggregates over the request log. A query's range is cut into pieces aligned to the rollup
//! grains, coarsest first: whole days, hours and minutes are read from the rollups, and only the
//! partial minutes at the edges from the records. Queries whose filters or dimensions the rollups do
//! not carry read the records of the range. Merging sketches is exact, so every path gives the same
//! figures.

use std::collections::{BTreeMap, BTreeSet};

use antirion_store::StoreError;
use antirion_store::logs::{
    CACHE_PREFIX_FROM, LogFilter, LogOrder, ROLLUP_DIMS, RollupQuery, RollupRow, ScopeRef, SortKey, StoredLog,
};
use serde_json::Value;

use super::facts::{Extras, Facts, d};
use super::rollup::{Agg, GRAINS};
use crate::Runtime;

/// Records read per store call when a range is aggregated from records.
const SCAN_BATCH: usize = 2_000;

/// A dimension analytics group by.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Dim {
    Model,
    Provider,
    Deployment,
    Organization,
    Team,
    Project,
    Key,
    EndUser,
    Tag,
    StatusClass,
    Route,
    RouteNode,
    RequestedModel,
    User,
    CacheStatus,
    Guardrail,
    GuardrailCategory,
    GuardrailVerdict,
    Severity,
    ClientIp,
    ClientCountry,
    Environment,
    /// The prompt prefix a request's provider prompt caching works on.
    CachePrefix,
}

impl Dim {
    #[must_use]
    pub fn parse(s: &str) -> Option<Dim> {
        Some(match s {
            "model" => Dim::Model,
            "provider" => Dim::Provider,
            "deployment" => Dim::Deployment,
            "organization" => Dim::Organization,
            "team" => Dim::Team,
            "project" => Dim::Project,
            "key" => Dim::Key,
            "end_user" => Dim::EndUser,
            "tag" => Dim::Tag,
            "status_class" => Dim::StatusClass,
            "route" => Dim::Route,
            "route_node" => Dim::RouteNode,
            "requested_model" => Dim::RequestedModel,
            "user" => Dim::User,
            "cache_status" => Dim::CacheStatus,
            "guardrail" => Dim::Guardrail,
            "guardrail_category" => Dim::GuardrailCategory,
            "guardrail_verdict" => Dim::GuardrailVerdict,
            "severity" => Dim::Severity,
            "client_ip" => Dim::ClientIp,
            "client_country" => Dim::ClientCountry,
            "environment" => Dim::Environment,
            "cache_prefix" => Dim::CachePrefix,
            _ => return None,
        })
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Dim::Model => "model",
            Dim::Provider => "provider",
            Dim::Deployment => "deployment",
            Dim::Organization => "organization",
            Dim::Team => "team",
            Dim::Project => "project",
            Dim::Key => "key",
            Dim::EndUser => "end_user",
            Dim::Tag => "tag",
            Dim::StatusClass => "status_class",
            Dim::Route => "route",
            Dim::RouteNode => "route_node",
            Dim::RequestedModel => "requested_model",
            Dim::User => "user",
            Dim::CacheStatus => "cache_status",
            Dim::Guardrail => "guardrail",
            Dim::GuardrailCategory => "guardrail_category",
            Dim::GuardrailVerdict => "guardrail_verdict",
            Dim::Severity => "severity",
            Dim::ClientIp => "client_ip",
            Dim::ClientCountry => "client_country",
            Dim::Environment => "environment",
            Dim::CachePrefix => "cache_prefix",
        }
    }

    /// The rollup column that holds this dimension; `None` for those only records have.
    #[must_use]
    pub fn rollup_column(self) -> Option<usize> {
        let column = match self {
            Dim::Model => "model",
            Dim::Provider => "provider",
            Dim::Deployment => "deployment",
            Dim::Organization => "organization",
            Dim::Team => "team",
            Dim::Project => "project",
            Dim::Key => "key_name",
            Dim::StatusClass => "status_code",
            Dim::Route => "route",
            Dim::RouteNode => "route_node",
            Dim::RequestedModel => "requested_model",
            Dim::User => "user_name",
            Dim::CacheStatus => "cache_status",
            Dim::Environment => "environment",
            Dim::CachePrefix => "cache_prefix",
            _ => return None,
        };
        ROLLUP_DIMS.iter().position(|c| *c == column)
    }
}

fn status_class(code: Option<&str>) -> Option<String> {
    code.and_then(|c| c.chars().next())
        .filter(char::is_ascii_digit)
        .map(|c| format!("{c}xx"))
}

/// The rollup side of a selection: the same records as the log filter, on rollup columns.
#[derive(Clone, Debug, Default)]
pub struct RollupFilter {
    pub any_of: Vec<(usize, Vec<String>)>,
    pub status_classes: Vec<u8>,
    pub scopes: Option<Vec<ScopeRef>>,
}

/// One aggregate query.
#[derive(Clone, Debug, Default)]
pub struct AggQuery {
    pub from_us: i64,
    pub to_us: i64,
    /// Bucket starts in increasing order; each bucket ends where the next starts (the last at
    /// `to_us`). The first is at or before `from_us`.
    pub starts: Vec<i64>,
    pub group_by: Vec<Dim>,
    pub filter: LogFilter,
    /// `None` when a filter only records can answer.
    pub rollup: Option<RollupFilter>,
    /// `metadata.<name>` filters, matched on the records.
    pub metadata: Vec<(String, String)>,
    /// Keep the largest groups by requests and fold the rest into `_other`.
    pub top: Option<usize>,
}

/// One bucket of one group.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Group {
    pub start_us: i64,
    pub key: Vec<Option<String>>,
    pub agg: Agg,
    /// The distinct keys counted in the bucket.
    pub keys: BTreeSet<String>,
}

/// The pieces of `[from, to)`: `(Some(grain), …)` aligned to a grain, `(None, …)` for records.
#[must_use]
pub fn segments(from: i64, to: i64, grains: &[i64]) -> Vec<(Option<i64>, i64, i64)> {
    if from >= to {
        return Vec::new();
    }
    let Some((g, rest)) = grains.split_first() else {
        return vec![(None, from, to)];
    };
    let unit = g.saturating_mul(1_000_000);
    let a = from.div_euclid(unit) * unit + if from.rem_euclid(unit) == 0 { 0 } else { unit };
    let b = to.div_euclid(unit) * unit;
    if a >= b {
        return segments(from, to, rest);
    }
    let mut out = segments(from, a, rest);
    out.push((Some(*g), a, b));
    out.extend(segments(b, to, rest));
    out
}

/// The coarsest grain every interior bucket boundary is aligned to.
fn coarsest_grain(starts: &[i64]) -> Option<usize> {
    GRAINS.iter().rposition(|g| {
        let unit = g * 1_000_000;
        starts.iter().skip(1).all(|s| s.rem_euclid(unit) == 0)
    })
}

/// Whether a record's metadata matches every `metadata.<name>` filter.
#[must_use]
pub fn metadata_matches(record: &Value, filters: &[(String, String)]) -> bool {
    filters.iter().all(|(k, want)| {
        let path = k.strip_prefix("metadata.").unwrap_or(k);
        let member = path
            .split('.')
            .try_fold(record.get("metadata").unwrap_or(&Value::Null), |v, k| v.get(k));
        match member {
            Some(Value::String(s)) => s == want,
            Some(Value::Number(n)) => n.to_string() == *want,
            Some(Value::Bool(b)) => b.to_string() == *want,
            _ => false,
        }
    })
}

type Table = BTreeMap<(i64, Vec<Option<String>>), (Agg, BTreeSet<String>)>;

struct Grouper<'a> {
    q: &'a AggQuery,
    table: Table,
}

impl Grouper<'_> {
    fn bucket(&self, t_us: i64) -> Option<i64> {
        if t_us < self.q.from_us || t_us >= self.q.to_us {
            return None;
        }
        let i = self.q.starts.partition_point(|s| *s <= t_us);
        self.q.starts.get(i.checked_sub(1)?).copied()
    }

    fn add_row(&mut self, row: &RollupRow) {
        let Some(start) = self.bucket(row.start_us) else {
            return;
        };
        let dim = |i: usize| row.dims.get(i).cloned().flatten();
        let key: Vec<Option<String>> = self
            .q
            .group_by
            .iter()
            .map(|g| match g {
                Dim::StatusClass => status_class(dim(d::STATUS_CODE).as_deref()),
                other => other.rollup_column().and_then(dim),
            })
            .collect();
        let slot = self.table.entry((start, key)).or_default();
        slot.0.merge(&Agg::of_row(row));
        slot.1.extend(dim(d::KEY));
    }

    fn add_facts(&mut self, f: &Facts) {
        let Some(start) = self.bucket(f.started_us) else {
            return;
        };
        let dim = |i: usize| f.dims.get(i).cloned().flatten();
        let many = |v: &[String]| -> Vec<Option<String>> {
            if v.is_empty() {
                vec![None]
            } else {
                let set: BTreeSet<&String> = v.iter().collect();
                set.into_iter().cloned().map(Some).collect()
            }
        };
        let mut keys: Vec<Vec<Option<String>>> = vec![Vec::new()];
        for g in &self.q.group_by {
            let values = match g {
                Dim::EndUser => vec![f.end_user.clone()],
                Dim::Tag => many(&f.tags),
                Dim::StatusClass => vec![status_class(dim(d::STATUS_CODE).as_deref())],
                Dim::Guardrail => many(&f.guardrails),
                Dim::GuardrailCategory => many(&f.categories),
                Dim::GuardrailVerdict => many(&f.verdicts),
                Dim::Severity => many(&f.severities),
                Dim::ClientIp => vec![f.client_ip.clone()],
                Dim::ClientCountry => vec![f.client_country.clone()],
                Dim::CachePrefix => vec![f.cache_prefix.clone()],
                other => vec![other.rollup_column().and_then(dim)],
            };
            keys = keys
                .into_iter()
                .flat_map(|k| {
                    values.iter().map(move |v| {
                        let mut k = k.clone();
                        k.push(v.clone());
                        k
                    })
                })
                .collect();
        }
        for key in keys {
            let slot = self.table.entry((start, key)).or_default();
            slot.0.add(f);
            slot.1.extend(dim(d::KEY));
        }
    }

    fn finish(self) -> Vec<Group> {
        let mut table = self.table;
        let width = self.q.group_by.len();
        if let Some(n) = self.q.top {
            let mut totals: BTreeMap<Vec<Option<String>>, i64> = BTreeMap::new();
            for ((_, key), (agg, _)) in &table {
                *totals.entry(key.clone()).or_default() += agg.counts[0];
            }
            let mut ranked: Vec<(Vec<Option<String>>, i64)> = totals.into_iter().collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let kept: BTreeSet<Vec<Option<String>>> = ranked.into_iter().take(n).map(|(k, _)| k).collect();
            let other = vec![Some("_other".to_owned()); width];
            let mut folded = Table::new();
            for ((start, key), (agg, keys)) in table {
                let key = if kept.contains(&key) { key } else { other.clone() };
                let slot = folded.entry((start, key)).or_default();
                slot.0.merge(&agg);
                slot.1.extend(keys);
            }
            table = folded;
        }
        if width == 0 {
            for s in &self.q.starts {
                table.entry((*s, Vec::new())).or_default();
            }
        }
        table
            .into_iter()
            .map(|((start_us, key), (agg, keys))| Group {
                start_us,
                key,
                agg,
                keys,
            })
            .collect()
    }
}

impl Runtime {
    /// The buckets of `q`, by start then group values.
    pub async fn aggregate(&self, q: &AggQuery) -> Result<Vec<Group>, StoreError> {
        self.read_logs().await;
        let mut g = Grouper {
            q,
            table: Table::new(),
        };
        let rollups = q
            .rollup
            .as_ref()
            .filter(|_| q.metadata.is_empty() && q.group_by.iter().all(|d| d.rollup_column().is_some()));
        let grains: Vec<i64> = match (rollups, coarsest_grain(&q.starts)) {
            (Some(_), Some(i)) => GRAINS.iter().take(i + 1).rev().copied().collect(),
            _ => Vec::new(),
        };
        // Rollups carry a prompt prefix only from the version that added it; earlier periods are
        // read from the records.
        let rolled_from = if q.group_by.contains(&Dim::CachePrefix) {
            self.store
                .get_or_init(CACHE_PREFIX_FROM, b"0".to_vec())
                .await
                .ok()
                .and_then(|v| String::from_utf8(v).ok())
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(i64::MAX)
        } else {
            q.from_us
        };
        // Records before the watermark's next minute are scanned; rollups serve the rest.
        let pieces = if rolled_from <= q.from_us {
            segments(q.from_us, q.to_us, &grains)
        } else {
            let split = rolled_from
                .saturating_add(MINUTE_US - 1)
                .div_euclid(MINUTE_US)
                .saturating_mul(MINUTE_US)
                .min(q.to_us);
            let mut pieces = vec![(None, q.from_us, split)];
            pieces.extend(segments(split, q.to_us, &grains));
            pieces
        };
        for (grain, from, to) in pieces {
            match (grain, rollups) {
                (Some(grain_s), Some(r)) => {
                    let rows = self
                        .store
                        .rollups(RollupQuery {
                            grain_s,
                            from_us: from,
                            to_us: to,
                            any_of: r.any_of.clone(),
                            status_classes: r.status_classes.clone(),
                            scopes: r.scopes.clone(),
                        })
                        .await?;
                    for row in &rows {
                        g.add_row(row);
                    }
                }
                _ => self.scan(q, from, to, &mut |f| g.add_facts(f)).await?,
            }
        }
        Ok(g.finish())
    }

    async fn scan(
        &self,
        q: &AggQuery,
        from: i64,
        to: i64,
        add: &mut (dyn FnMut(&Facts) + Send),
    ) -> Result<(), StoreError> {
        let mut filter = q.filter.clone();
        filter.from_us = Some(filter.from_us.map_or(from, |f| f.max(from)));
        filter.to_us = Some(filter.to_us.map_or(to, |t| t.min(to)));
        let mut order = LogOrder {
            sort: SortKey::StartedAt,
            desc: false,
            after: None,
            limit: SCAN_BATCH,
        };
        loop {
            let page = self.list_stored(filter.clone(), order.clone()).await?;
            let full = page.len() == SCAN_BATCH;
            for l in &page {
                if let Some(f) = facts_of(&l.log, &q.metadata) {
                    add(&f);
                }
            }
            match page.last() {
                Some(last) if full => order.after = Some(last.pos.clone()),
                _ => return Ok(()),
            }
        }
    }
}

const MINUTE_US: i64 = 60_000_000;

/// The facts of a stored record, when it matches the metadata filters.
fn facts_of(log: &StoredLog, metadata: &[(String, String)]) -> Option<Facts> {
    let record: Value = serde_json::from_str(&log.record).ok()?;
    if !metadata_matches(&record, metadata) {
        return None;
    }
    let extras = Extras {
        user: log.user.clone(),
        route_node: log.route_node.clone(),
    };
    Some(Facts::of(&record, &extras, log.environment.clone()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const M: i64 = 60_000_000;
    const H: i64 = 60 * M;

    #[test]
    fn ranges_are_cut_into_aligned_pieces() {
        let s = segments(30_000_000, 2 * H + 90_000_000, &[3_600, 60]);
        assert_eq!(
            s,
            vec![
                (None, 30_000_000, M),
                (Some(60), M, H),
                (Some(3_600), H, 2 * H),
                (Some(60), 2 * H, 2 * H + M),
                (None, 2 * H + M, 2 * H + 90_000_000),
            ]
        );
        assert_eq!(segments(5, 5, &[60]), Vec::new());
        assert_eq!(segments(0, 10, &[]), vec![(None, 0, 10)]);
    }

    #[test]
    fn the_grain_follows_the_bucket_boundaries() {
        assert_eq!(coarsest_grain(&[0, 5 * M]), Some(0));
        assert_eq!(coarsest_grain(&[7, H, 2 * H]), Some(1));
        assert_eq!(coarsest_grain(&[0]), Some(2));
        assert_eq!(coarsest_grain(&[0, 30_000_000]), None);
    }

    fn facts(t: i64, model: &str, tags: &[&str]) -> Facts {
        let mut dims = vec![None; ROLLUP_DIMS.len()];
        dims[5] = Some(model.to_owned());
        dims[d::KEY] = Some(format!("k-{model}"));
        dims[d::STATUS_CODE] = Some("200".to_owned());
        let mut f = Facts {
            started_us: t,
            dims,
            latency_ms: 10.0,
            tags: tags.iter().map(|s| (*s).to_owned()).collect(),
            ..Facts::default()
        };
        f.counts[0] = 1;
        f
    }

    #[test]
    fn groups_buckets_and_folds_the_rest() {
        let q = AggQuery {
            from_us: 0,
            to_us: 10 * M,
            starts: vec![0, 5 * M],
            group_by: vec![Dim::Model, Dim::Tag],
            top: Some(1),
            ..AggQuery::default()
        };
        let mut g = Grouper {
            q: &q,
            table: Table::new(),
        };
        g.add_facts(&facts(M, "a", &["x", "y"]));
        g.add_facts(&facts(6 * M, "a", &["x"]));
        g.add_facts(&facts(6 * M, "b", &[]));
        g.add_facts(&facts(11 * M, "b", &[]));
        let out = g.finish();
        let keys: Vec<(i64, Vec<Option<String>>, i64)> = out
            .iter()
            .map(|g| (g.start_us, g.key.clone(), g.agg.counts[0]))
            .collect();
        let s = |v: &str| Some(v.to_owned());
        assert_eq!(
            keys,
            vec![
                (0, vec![s("_other"), s("_other")], 1),
                (0, vec![s("a"), s("x")], 1),
                (5 * M, vec![s("_other"), s("_other")], 1),
                (5 * M, vec![s("a"), s("x")], 1),
            ]
        );
        assert_eq!(out[1].keys.len(), 1);
    }

    #[test]
    fn an_ungrouped_query_has_every_bucket() {
        let q = AggQuery {
            from_us: 0,
            to_us: 15 * M,
            starts: vec![0, 5 * M, 10 * M],
            ..AggQuery::default()
        };
        let g = Grouper {
            q: &q,
            table: Table::new(),
        };
        assert_eq!(g.finish().len(), 3);
    }

    #[test]
    fn metadata_filters_match_nested_members() {
        let r = json!({"metadata": {"run": "r1", "n": 3, "deep": {"a": true}}});
        assert!(metadata_matches(&r, &[("metadata.run".into(), "r1".into())]));
        assert!(metadata_matches(
            &r,
            &[
                ("metadata.n".into(), "3".into()),
                ("metadata.deep.a".into(), "true".into())
            ]
        ));
        assert!(!metadata_matches(&r, &[("metadata.run".into(), "r2".into())]));
    }
}

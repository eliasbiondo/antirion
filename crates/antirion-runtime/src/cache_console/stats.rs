//! Semantic statistics: hits, their summed similarity and false-positive reports, tallied per
//! group and day in node memory and added to the store's per-node rows; read over the trailing 30
//! days for every configuration level that turns the semantic layer on.

use serde_json::Value;

use super::{DAY_MS, Tally};
use crate::Runtime;

/// The window the statistics cover, in days, today included.
const WINDOW_DAYS: i64 = 30;

/// One group's statistics.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticStats {
    /// `gateway`, or `<level>/<name>` of the level that turns the semantic layer on.
    pub scope: String,
    pub embedding_model: Option<String>,
    pub threshold: f64,
    pub entries: u64,
    pub hits: u64,
    pub mean_similarity_of_hits: Option<f64>,
    pub false_positive_reports: u64,
    pub false_positive_rate: Option<f64>,
}

/// Every level whose `policies.cache.semantic.enabled` is true, with the semantic block merged
/// from the gateway's.
fn groups(doc: &Value) -> Vec<(String, Value)> {
    let gateway = doc
        .pointer("/policies/cache/semantic")
        .cloned()
        .unwrap_or(Value::Null);
    let merged = |own: &Value| {
        let mut out = gateway.as_object().cloned().unwrap_or_default();
        for (k, v) in own.as_object().into_iter().flatten() {
            out.insert(k.clone(), v.clone());
        }
        Value::Object(out)
    };
    let mut out = Vec::new();
    if gateway.get("enabled") == Some(&Value::Bool(true)) {
        out.push(("gateway".to_owned(), gateway.clone()));
    }
    for (collection, level) in [
        ("organizations", "organization"),
        ("teams", "team"),
        ("projects", "project"),
        ("keys", "key"),
        ("routes", "route"),
        ("models", "model"),
    ] {
        for (name, r) in doc
            .get(collection)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if let Some(own) = r.pointer("/policies/cache/semantic")
                && own.get("enabled") == Some(&Value::Bool(true))
            {
                out.push((format!("{level}/{name}"), merged(own)));
            }
        }
    }
    out
}

impl Runtime {
    fn day(&self) -> i64 {
        (self.now().0 / 1000).div_euclid(DAY_MS)
    }

    fn tally(&self, group: &str, f: impl FnOnce(&mut Tally)) {
        let day = self.day();
        let mut t = self.state.cache_console.tallies.lock();
        f(t.entry((group.to_owned(), day)).or_default());
    }

    /// Counts a semantic hit of `group` with its similarity.
    pub fn semantic_hit(&self, group: &str, similarity: f64) {
        let micros = (similarity * 1e6).round() as i64;
        self.tally(group, |t| {
            t.hits += 1;
            t.similarity_micros += micros;
        });
    }

    /// Counts a false-positive report of `group`.
    pub fn semantic_report(&self, group: &str) {
        self.tally(group, |t| t.reports += 1);
    }

    /// Adds what this node counted to the store.
    pub async fn flush_semantic_tallies(&self) {
        let taken = std::mem::take(&mut *self.state.cache_console.tallies.lock());
        if taken.is_empty() {
            return;
        }
        let rows: Vec<antirion_store::SemanticTally> = taken
            .iter()
            .map(|((group, day), t)| antirion_store::SemanticTally {
                group: group.clone(),
                day: *day,
                hits: t.hits,
                similarity_micros: t.similarity_micros,
                reports: t.reports,
            })
            .collect();
        if let Err(e) = self.store.add_semantic_tallies(&self.node.id, rows).await {
            tracing::debug!(error = %e, "semantic cache tallies could not be stored; kept for later");
            let mut t = self.state.cache_console.tallies.lock();
            for (k, v) in taken {
                let slot = t.entry(k).or_default();
                slot.hits += v.hits;
                slot.similarity_micros += v.similarity_micros;
                slot.reports += v.reports;
            }
        }
    }

    /// The statistics of every group the configuration enables, over the trailing 30 days.
    pub async fn semantic_stats(&self) -> Vec<SemanticStats> {
        self.flush_semantic_tallies().await;
        let snap = self.gens.load();
        let since = self.day() - (WINDOW_DAYS - 1);
        let sums = self.store.semantic_tallies(since).await.unwrap_or_default();
        let now_ms = self.now().0 / 1000;
        let mut out = Vec::new();
        for (scope, block) in groups(&snap.config.doc) {
            let sum = sums.iter().find(|s| s.group == scope);
            let hits = sum.map_or(0, |s| s.hits.max(0));
            let reports = sum.map_or(0, |s| s.reports.max(0));
            let entries = self
                .state
                .cache
                .count_dim(&antirion_cache::meta::dim("semantic", &scope), now_ms)
                .await
                .unwrap_or(0);
            out.push(SemanticStats {
                embedding_model: block
                    .get("embedding_model")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                threshold: block.get("threshold").and_then(Value::as_f64).unwrap_or(0.95),
                entries,
                hits: u64::try_from(hits).unwrap_or(0),
                mean_similarity_of_hits: (hits > 0)
                    .then(|| sum.map_or(0, |s| s.similarity_micros) as f64 / 1e6 / hits as f64),
                false_positive_reports: u64::try_from(reports).unwrap_or(0),
                false_positive_rate: (hits > 0).then(|| reports as f64 / hits as f64),
                scope,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn groups_are_the_levels_that_enable_the_layer() {
        let doc = json!({
            "policies": {"cache": {"semantic": {"enabled": true, "embedding_model": "e", "threshold": 0.9}}},
            "models": {"m": {"policies": {"cache": {"semantic": {"enabled": true, "threshold": 0.97}}}},
                       "n": {"policies": {"cache": {"semantic": {"enabled": false}}}}}
        });
        let g = groups(&doc);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].0, "gateway");
        assert_eq!(g[1].0, "model/m");
        assert_eq!(g[1].1["embedding_model"], "e");
        assert_eq!(g[1].1["threshold"], 0.97);
    }
}

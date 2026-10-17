//! Key anomaly flags. Each authenticated request adds what it was (time, client network, user
//! agent family) to a sharded in-memory tally; a background task drains the tally every second,
//! compares each key against its trailing baseline in the store, opens flags, and writes the
//! key's history (networks, families and countries seen, requests per hour) in batches. Nothing
//! here can refuse or slow a request.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use antirion_config::scalar::parse_duration;
use antirion_core::{ResourceUid, UtcMicros};
use antirion_store::{AnomalyRow, SeenRow};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::Runtime;

const SHARDS: usize = 16;
const HOUR_US: i64 = 3_600_000_000;
/// A key's first hour of use is learnt, never flagged.
const LEARNING_US: i64 = HOUR_US;
/// How long a key's loaded history is trusted before it is read again (other nodes add to it).
const PROFILE_TTL_US: i64 = 60_000_000;
/// How long another node's hourly count is trusted before it is read again.
const HOURS_TTL_US: i64 = 10_000_000;
const EVALUATE_EVERY: Duration = Duration::from_secs(1);
/// The audit actor of flags.
const ACTOR: &str = "key_anomalies";
/// The alert rule a flag's instance belongs to.
pub const ALERT_RULE: &str = "key_anomaly";

/// `auth.anomaly_detection`.
#[derive(Clone, Debug, PartialEq)]
pub struct AnomalySettings {
    pub enabled: bool,
    pub baseline: Duration,
    pub spike_factor: f64,
    pub min_requests: u64,
    pub new_network: bool,
    pub new_user_agent: bool,
    pub new_country: bool,
}

impl Default for AnomalySettings {
    fn default() -> AnomalySettings {
        AnomalySettings {
            enabled: true,
            baseline: Duration::from_secs(7 * 86_400),
            spike_factor: 5.0,
            min_requests: 100,
            new_network: true,
            new_user_agent: true,
            new_country: true,
        }
    }
}

impl AnomalySettings {
    /// The settings of a resolved document.
    #[must_use]
    pub fn of(resolved: &Value) -> AnomalySettings {
        let d = AnomalySettings::default();
        let Some(a) = resolved.pointer("/auth/anomaly_detection") else {
            return d;
        };
        let flag = |k: &str, default: bool| a.get(k).and_then(Value::as_bool).unwrap_or(default);
        AnomalySettings {
            enabled: flag("enabled", d.enabled),
            baseline: a
                .get("baseline")
                .and_then(Value::as_str)
                .and_then(parse_duration)
                .unwrap_or(d.baseline),
            spike_factor: a
                .get("spike_factor")
                .and_then(Value::as_f64)
                .unwrap_or(d.spike_factor),
            min_requests: a
                .get("min_requests")
                .and_then(Value::as_u64)
                .unwrap_or(d.min_requests),
            new_network: flag("new_network", d.new_network),
            new_user_agent: flag("new_user_agent", d.new_user_agent),
            new_country: flag("new_country", d.new_country),
        }
    }
}

/// A client network: an IPv4 /24 or an IPv6 /48.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum Net {
    V4(u32),
    V6(u64),
}

impl Net {
    fn of(ip: IpAddr) -> Net {
        match ip {
            IpAddr::V4(v4) => Net::V4(u32::from(v4) & 0xffff_ff00),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Net::V4(u32::from(v4) & 0xffff_ff00),
                None => Net::V6(u64::try_from(u128::from(v6) >> 80).unwrap_or(0) << 16),
            },
        }
    }

    fn text(self) -> String {
        match self {
            Net::V4(n) => format!("{}/24", std::net::Ipv4Addr::from(n)),
            Net::V6(n) => format!("{}/48", std::net::Ipv6Addr::from(u128::from(n) << 64)),
        }
    }
}

/// The family of a `User-Agent` value: the name of its first product token, lowercased
/// (`python-requests/2.31` → `python-requests`); `none` without the header.
fn family(ua: Option<&[u8]>) -> String {
    let Some(ua) = ua else {
        return "none".to_owned();
    };
    let name: String = ua
        .iter()
        .take_while(|b| !matches!(b, b'/' | b' ' | b'\t' | b';' | b'('))
        .take(64)
        .map(|b| char::from(*b).to_ascii_lowercase())
        .filter(char::is_ascii_graphic)
        .collect();
    if name.is_empty() { "none".to_owned() } else { name }
}

fn hash_of(bytes: Option<&[u8]>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// What one key did since the last evaluation.
#[derive(Debug, Default)]
struct Tally {
    name: String,
    /// Requests per hour (`hour_us` → count).
    hours: HashMap<i64, u64>,
    networks: HashMap<Net, (u64, IpAddr)>,
    /// By a hash of the raw header, with the family it names.
    families: HashMap<u64, (u64, String)>,
    last_us: i64,
}

/// What the store and this node know of a key's history.
#[derive(Debug, Default)]
struct Profile {
    loaded_us: i64,
    first_us: Option<i64>,
    /// `(kind, subject)` → last seen.
    seen: HashMap<(String, String), i64>,
    /// Open flags by `(kind, subject)`.
    open: HashSet<(String, String)>,
    /// This node's own requests per hour.
    own_hours: HashMap<i64, u64>,
    /// Every node's requests per hour of the baseline, and when that was read.
    hours: HashMap<i64, i64>,
    hours_us: i64,
    /// The last hour a spike was raised for.
    spiked: Option<i64>,
}

/// The detector's state on one node.
#[derive(Debug, Default)]
pub struct Detector {
    shards: [Mutex<HashMap<ResourceUid, Tally>>; SHARDS],
    profiles: Mutex<HashMap<ResourceUid, Profile>>,
    pruned_hour: Mutex<i64>,
}

impl Detector {
    fn shard(&self, key: ResourceUid) -> &Mutex<HashMap<ResourceUid, Tally>> {
        let [first, ..] = &self.shards;
        let i = usize::try_from(key.0 % SHARDS as u128).unwrap_or(0);
        self.shards.get(i).unwrap_or(first)
    }

    /// Adds one authenticated request of `key`.
    pub fn observe(
        &self,
        key: ResourceUid,
        name: &str,
        now: UtcMicros,
        ip: Option<IpAddr>,
        ua: Option<&[u8]>,
    ) {
        let hour = now.0 - now.0.rem_euclid(HOUR_US);
        let mut shard = self.shard(key).lock();
        let t = shard.entry(key).or_default();
        if t.name.is_empty() {
            t.name = name.to_owned();
        }
        *t.hours.entry(hour).or_default() += 1;
        t.last_us = t.last_us.max(now.0);
        if let Some(ip) = ip {
            t.networks.entry(Net::of(ip)).or_insert((0, ip)).0 += 1;
        }
        t.families.entry(hash_of(ua)).or_insert_with(|| (0, family(ua))).0 += 1;
    }

    fn drain(&self) -> Vec<(ResourceUid, Tally)> {
        let mut out = Vec::new();
        for shard in &self.shards {
            out.extend(std::mem::take(&mut *shard.lock()));
        }
        out
    }

    pub fn clear(&self) {
        for shard in &self.shards {
            shard.lock().clear();
        }
        self.profiles.lock().clear();
    }
}

/// Something to flag on a key.
pub(crate) struct Finding<'a> {
    pub(crate) uid: ResourceUid,
    pub(crate) key: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) subject: &'a str,
    pub(crate) count: u64,
    pub(crate) at_us: i64,
    pub(crate) detail: Value,
}

/// One thing a key was seen with during an interval.
struct Sighting {
    kind: &'static str,
    subject: String,
    count: u64,
    detail: Value,
}

fn severity(kind: &str) -> &'static str {
    match kind {
        "country" | "exposed" => "critical",
        "user_agent" => "info",
        _ => "warning",
    }
}

/// A flag as the Admin API shows it.
#[must_use]
pub fn flag_json(a: &AnomalyRow) -> Value {
    let detail: Value = serde_json::from_str(&a.detail).unwrap_or(Value::Null);
    json!({
        "id": a.id,
        "key": a.key_name,
        "owner": a.owner,
        "kind": a.kind,
        "severity": a.severity,
        "first_seen": UtcMicros(a.first_us).to_rfc3339_seconds(),
        "last_seen": UtcMicros(a.last_us).to_rfc3339_seconds(),
        "count": a.count,
        "detail": detail,
        "state": a.state,
    })
}

impl Runtime {
    /// The number of open flags of each key, by key name.
    pub async fn open_flags_by_key(&self) -> HashMap<String, u64> {
        self.store.open_anomaly_counts().await.unwrap_or_default()
    }

    /// Reads a key's history from the store when this node has none or it is stale.
    async fn profile(&self, uid: ResourceUid, name: &str, now: i64) {
        let fresh = self
            .state
            .anomalies
            .profiles
            .lock()
            .get(&uid)
            .is_some_and(|p| now - p.loaded_us < PROFILE_TTL_US && p.loaded_us <= now);
        if fresh {
            return;
        }
        let hex = uid.to_hex();
        let Ok(rows) = self.store.key_seen(&hex).await else {
            return;
        };
        let open = self
            .store
            .anomalies(&antirion_store::AnomalyFilter {
                key: Some(name.to_owned()),
                state: Some("open".to_owned()),
                limit: 10_000,
                ..antirion_store::AnomalyFilter::default()
            })
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.key_uid == hex)
            .map(|a| (a.kind, a.subject))
            .collect();
        let mut profiles = self.state.anomalies.profiles.lock();
        let p = profiles.entry(uid).or_default();
        p.loaded_us = now;
        p.open = open;
        for (kind, subject, first, last) in rows {
            if kind == "first" {
                p.first_us = Some(p.first_us.map_or(first, |f| f.min(first)));
            } else {
                let e = p.seen.entry((kind, subject)).or_insert(last);
                *e = (*e).max(last);
            }
        }
    }

    /// Every node's requests per hour of a key from `from_us` on, read again when stale.
    async fn hours(&self, uid: ResourceUid, from_us: i64, now: i64) -> HashMap<i64, i64> {
        let cached = {
            let profiles = self.state.anomalies.profiles.lock();
            profiles
                .get(&uid)
                .filter(|p| now - p.hours_us < HOURS_TTL_US && p.hours_us <= now)
                .map(|p| p.hours.clone())
        };
        if let Some(h) = cached {
            return h;
        }
        let hours = self
            .store
            .key_hours(&uid.to_hex(), from_us)
            .await
            .unwrap_or_default();
        if let Some(p) = self.state.anomalies.profiles.lock().get_mut(&uid) {
            p.hours.clone_from(&hours);
            p.hours_us = now;
        }
        hours
    }

    /// Drains the tally and evaluates every key that was used.
    pub async fn evaluate_anomalies(&self) {
        let snap = self.gens.load();
        let settings = snap.identity.anomaly.clone();
        let drained = self.state.anomalies.drain();
        if !settings.enabled || drained.is_empty() {
            return;
        }
        let now = self.now().0;
        let baseline_us = i64::try_from(settings.baseline.as_micros()).unwrap_or(i64::MAX);
        let mut seen_rows = Vec::new();
        let mut hour_rows = Vec::new();
        for (uid, tally) in drained {
            self.profile(uid, &tally.name, now).await;
            let hex = uid.to_hex();
            let (learning, sightings) = {
                let mut profiles = self.state.anomalies.profiles.lock();
                let p = profiles.entry(uid).or_default();
                let new = p.first_us.is_none();
                let first = *p.first_us.get_or_insert(tally.last_us);
                if new {
                    seen_rows.push(SeenRow {
                        key_uid: hex.clone(),
                        kind: "first".to_owned(),
                        subject: String::new(),
                        at_us: first,
                    });
                }
                for (hour, n) in &tally.hours {
                    let own = p.own_hours.entry(*hour).or_default();
                    *own += n;
                    hour_rows.push((hex.clone(), *hour, i64::try_from(*own).unwrap_or(i64::MAX)));
                }
                let mut sightings = Vec::new();
                for (net, (count, ip)) in &tally.networks {
                    sightings.push(Sighting {
                        kind: "network",
                        subject: net.text(),
                        count: *count,
                        detail: json!({"network": net.text(), "client_ip": ip.to_string()}),
                    });
                    if let Some(country) = snap
                        .geoip
                        .as_ref()
                        .and_then(|g| g.locate(*ip))
                        .and_then(|g| g.get("country").and_then(Value::as_str).map(str::to_owned))
                    {
                        sightings.push(Sighting {
                            kind: "country",
                            subject: country.clone(),
                            count: *count,
                            detail: json!({"country": country, "network": net.text()}),
                        });
                    }
                }
                for (count, fam) in tally.families.values() {
                    sightings.push(Sighting {
                        kind: "user_agent",
                        subject: fam.clone(),
                        count: *count,
                        detail: json!({"user_agent_family": fam}),
                    });
                }
                let learning = tally.last_us - first < LEARNING_US;
                (learning, sightings)
            };
            for s in sightings {
                let key = (s.kind.to_owned(), s.subject.clone());
                let (known, open) = {
                    let profiles = self.state.anomalies.profiles.lock();
                    let p = profiles.get(&uid);
                    (
                        p.and_then(|p| p.seen.get(&key))
                            .is_some_and(|last| *last >= now - baseline_us),
                        p.is_some_and(|p| p.open.contains(&key)),
                    )
                };
                let watched = match s.kind {
                    "network" => settings.new_network,
                    "user_agent" => settings.new_user_agent,
                    _ => settings.new_country,
                };
                if watched && !learning && (!known || open) {
                    let finding = Finding {
                        uid,
                        key: &tally.name,
                        kind: s.kind,
                        subject: &s.subject,
                        count: s.count,
                        at_us: tally.last_us,
                        detail: s.detail,
                    };
                    self.raise_flag(&snap, finding).await;
                }
                seen_rows.push(SeenRow {
                    key_uid: hex.clone(),
                    kind: s.kind.to_owned(),
                    subject: s.subject.clone(),
                    at_us: tally.last_us,
                });
                if let Some(p) = self.state.anomalies.profiles.lock().get_mut(&uid) {
                    let e = p.seen.entry(key).or_insert(tally.last_us);
                    *e = (*e).max(tally.last_us);
                }
            }
            if !learning {
                self.check_spike(&snap, &settings, uid, &tally, now).await;
            }
        }
        if let Err(e) = self.store.put_key_seen(seen_rows).await {
            tracing::warn!(error = %e, "key history could not be written");
        }
        if let Err(e) = self.store.put_key_hours(&self.node.id, hour_rows).await {
            tracing::warn!(error = %e, "key request counts could not be written");
        }
        self.prune_key_history(now, baseline_us).await;
    }

    /// Flags the key when this hour's requests, every node's, exceed `spike_factor` times its
    /// hourly mean since the start of the baseline or its first use, whichever is later.
    async fn check_spike(
        &self,
        snap: &crate::Snapshot,
        settings: &AnomalySettings,
        uid: ResourceUid,
        tally: &Tally,
        now: i64,
    ) {
        let baseline_us = i64::try_from(settings.baseline.as_micros()).unwrap_or(i64::MAX);
        let hour = now - now.rem_euclid(HOUR_US);
        let mine = self
            .state
            .anomalies
            .profiles
            .lock()
            .get(&uid)
            .and_then(|p| p.own_hours.get(&hour).copied())
            .unwrap_or(0);
        if !tally.hours.contains_key(&hour) || mine == 0 {
            return;
        }
        let from = hour.saturating_sub(baseline_us);
        let hours = self.hours(uid, from, now).await;
        let others = {
            let stored = hours.get(&hour).copied().unwrap_or(0);
            let mine_stored = i64::try_from(mine).unwrap_or(i64::MAX);
            stored.saturating_sub(mine_stored).max(0)
        };
        let total = i64::try_from(mine).unwrap_or(i64::MAX).saturating_add(others);
        let (first, spiked) = {
            let profiles = self.state.anomalies.profiles.lock();
            let p = profiles.get(&uid);
            (p.and_then(|p| p.first_us), p.and_then(|p| p.spiked))
        };
        if spiked == Some(hour) || u64::try_from(total).unwrap_or(0) < settings.min_requests {
            return;
        }
        let start = from.max(first.map_or(from, |f| f - f.rem_euclid(HOUR_US)));
        let span = ((hour - start) / HOUR_US).max(1);
        let past: i64 = hours
            .iter()
            .filter(|(h, _)| **h >= start && **h < hour)
            .map(|(_, n)| *n)
            .sum();
        let mean = past as f64 / span as f64;
        let spike = total as f64 > settings.spike_factor * mean;
        if !spike {
            return;
        }
        if let Some(p) = self.state.anomalies.profiles.lock().get_mut(&uid) {
            p.spiked = Some(hour);
        }
        let detail = json!({
            "hour": UtcMicros(hour).to_rfc3339_seconds(),
            "requests": total,
            "baseline_hourly_mean": (mean * 100.0).round() / 100.0,
            "spike_factor": settings.spike_factor,
        });
        let finding = Finding {
            uid,
            key: &tally.name,
            kind: "spike",
            subject: "hour",
            count: 1,
            at_us: tally.last_us,
            detail,
        };
        self.raise_flag(snap, finding).await;
    }

    /// Opens a flag, or adds to the open flag of the same subject; a new flag is announced as
    /// `key.anomaly`, raised as a `security` alert instance and recorded in the audit log.
    pub(crate) async fn raise_flag(&self, snap: &crate::Snapshot, f: Finding<'_>) {
        let Finding {
            uid,
            key,
            kind,
            subject,
            count,
            at_us,
            detail,
        } = f;
        let id = &snap.identity;
        let Some(k) = id
            .keys_by_kid
            .values()
            .chain(id.imported.values())
            .map(|s| &s.key)
            .chain(id.jwt_keys.values())
            .find(|k| k.uid == uid)
            .cloned()
        else {
            return;
        };
        let row = AnomalyRow {
            id: antirion_core::RequestId::generate().as_str().to_owned(),
            key_uid: uid.to_hex(),
            key_name: key.to_owned(),
            owner: k.owner.clone(),
            organization: k.organization.clone(),
            team: k.team.clone(),
            project: k.project.clone(),
            kind: kind.to_owned(),
            subject: subject.to_owned(),
            severity: severity(kind).to_owned(),
            first_us: at_us,
            last_us: at_us,
            count: i64::try_from(count).unwrap_or(i64::MAX),
            detail: detail.to_string(),
            state: "open".to_owned(),
            dismissed_by: None,
            dismissed_us: None,
            reason: None,
        };
        let (flag, opened) = match self.store.raise_anomaly(row).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "an anomaly flag could not be stored");
                return;
            }
        };
        if let Some(p) = self.state.anomalies.profiles.lock().get_mut(&uid) {
            p.open.insert((kind.to_owned(), subject.to_owned()));
        }
        if !opened {
            return;
        }
        self.announce_flag(snap, &flag).await;
    }

    /// The event, alert instance and audit entry of a newly opened flag.
    pub async fn announce_flag(&self, snap: &crate::Snapshot, flag: &AnomalyRow) {
        let body = flag_json(flag);
        let data = json!({
            "anomaly": body, "key": flag.key_name, "owner": flag.owner,
            "organization": flag.organization, "team": flag.team, "project": flag.project,
        });
        self.send_alert(snap, None, "key.anomaly", data);
        let scope = [
            ("organization", &flag.organization),
            ("team", &flag.team),
            ("project", &flag.project),
            ("key", &flag.key_name),
            ("anomaly", &flag.id),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.clone()))
        .collect();
        let summary = format!("Key {} flagged: {} {}", flag.key_name, flag.kind, flag.subject);
        crate::alerting::raise_builtin(self, ALERT_RULE, "security", &flag.severity, scope, summary).await;
        self.audit_system(
            ACTOR,
            "keys.anomaly_flagged",
            &format!("keys/{}", flag.key_name),
            json!({"anomaly": flag.id, "kind": flag.kind, "subject": flag.subject}),
        )
        .await;
    }

    /// Dismisses an open flag and resolves its alert instance; `None` when the flag does not
    /// exist, `Some(false)` when it is not open.
    pub async fn dismiss_flag(&self, id: &str, actor: &str, reason: Option<&str>) -> Option<bool> {
        let now = self.now().0;
        let flag = self.store.anomaly(id).await.ok()??;
        let done = self.store.dismiss_anomaly(id, actor, reason, now).await.ok()?;
        if done {
            let scope = [
                ("organization", &flag.organization),
                ("team", &flag.team),
                ("project", &flag.project),
                ("key", &flag.key_name),
                ("anomaly", &flag.id),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.clone()))
            .collect();
            crate::alerting::resolve_builtin(self, ALERT_RULE, &scope, actor).await;
            if let Some(uid) = ResourceUid::from_hex(&flag.key_uid)
                && let Some(p) = self.state.anomalies.profiles.lock().get_mut(&uid)
            {
                p.open.remove(&(flag.kind.clone(), flag.subject.clone()));
            }
        }
        Some(done)
    }

    /// Drops key history older than the baseline, once per hour of gateway time.
    async fn prune_key_history(&self, now: i64, baseline_us: i64) {
        let hour = now - now.rem_euclid(HOUR_US);
        {
            let mut pruned = self.state.anomalies.pruned_hour.lock();
            if *pruned == hour {
                return;
            }
            *pruned = hour;
        }
        if let Err(e) = self
            .store
            .prune_key_history(hour.saturating_sub(baseline_us))
            .await
        {
            tracing::warn!(error = %e, "old key history could not be pruned");
        }
        for p in self.state.anomalies.profiles.lock().values_mut() {
            p.own_hours.retain(|h, _| *h >= hour.saturating_sub(baseline_us));
        }
    }
}

/// Evaluates the tally every second until the node stops.
pub(crate) async fn run(rt: Arc<Runtime>) {
    loop {
        tokio::select! {
            () = rt.shutdown.cancelled() => return,
            () = tokio::time::sleep(EVALUATE_EVERY) => {}
        }
        rt.evaluate_anomalies().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn networks_are_slash_24_and_slash_48() {
        let v4: IpAddr = "203.0.113.77".parse().unwrap();
        assert_eq!(Net::of(v4).text(), "203.0.113.0/24");
        let mapped: IpAddr = "::ffff:203.0.113.9".parse().unwrap();
        assert_eq!(Net::of(mapped), Net::of(v4));
        let v6: IpAddr = "2001:db8:abcd:12::1".parse().unwrap();
        assert_eq!(Net::of(v6).text(), "2001:db8:abcd::/48");
    }

    #[test]
    fn families_are_the_first_product_name() {
        assert_eq!(family(Some(b"python-requests/2.31")), "python-requests");
        assert_eq!(family(Some(b"OpenAI/Python 1.40.0")), "openai");
        assert_eq!(family(Some(b"curl")), "curl");
        assert_eq!(family(Some(b"")), "none");
        assert_eq!(family(None), "none");
    }

    #[test]
    fn the_tally_counts_per_key_hour_network_and_family() {
        let d = Detector::default();
        let k = ResourceUid::random();
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        d.observe(k, "k", UtcMicros(HOUR_US + 5), Some(ip), Some(b"curl/8"));
        d.observe(k, "k", UtcMicros(HOUR_US + 9), Some(ip), Some(b"curl/8"));
        d.observe(k, "k", UtcMicros(2 * HOUR_US), None, None);
        let drained = d.drain();
        let (_, t) = drained.first().unwrap();
        assert_eq!(t.hours.get(&HOUR_US), Some(&2));
        assert_eq!(t.hours.get(&(2 * HOUR_US)), Some(&1));
        assert_eq!(t.networks.len(), 1);
        assert_eq!(t.families.len(), 2);
        assert!(d.drain().is_empty());
    }

    #[test]
    fn settings_default_and_parse() {
        let s = AnomalySettings::of(
            &json!({"auth": {"anomaly_detection": {"min_requests": 5, "baseline": "1d"}}}),
        );
        assert!(s.enabled && s.min_requests == 5 && s.baseline == Duration::from_secs(86_400));
        assert_eq!(AnomalySettings::of(&json!({})), AnomalySettings::default());
    }
}

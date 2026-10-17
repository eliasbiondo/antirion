//! The effective settings of one request: gateway-wide `streaming`, and the `policies` members merged
//! from the broadest level to the narrowest, field by field.

use std::time::Duration;

use antirion_config::scalar::{parse_duration, parse_size};
use antirion_runtime::Snapshot;
use antirion_runtime::identity::KeyRt;
use serde_json::{Map, Value};

/// The `policies` objects of the levels that apply to a request, broadest first: gateway,
/// organization, team, project, key, route, model; with each level's name (`gateway`,
/// `project/p`).
pub(crate) struct Levels<'a>(Vec<Option<&'a Value>>, Vec<String>);

impl<'a> Levels<'a> {
    pub(crate) fn new(snap: &'a Snapshot, key: &KeyRt, model: &str, route: Option<&str>) -> Levels<'a> {
        let doc = &snap.config.doc;
        let at = |collection: &str, name: &str| {
            doc.get(collection)
                .and_then(|c| c.get(name))
                .and_then(|r| r.get("policies"))
        };
        let mut levels = Vec::with_capacity(7);
        let mut names = Vec::with_capacity(7);
        levels.push(doc.get("policies"));
        names.push("gateway".to_owned());
        for (collection, level, name) in [
            ("organizations", "organization", key.organization.as_str()),
            ("teams", "team", key.team.as_str()),
            ("projects", "project", key.project.as_str()),
            ("keys", "key", key.name.as_str()),
        ] {
            levels.push(at(collection, name));
            names.push(format!("{level}/{name}"));
        }
        if let Some(r) = route {
            levels.push(at("routes", r));
            names.push(format!("route/{r}"));
        }
        levels.push(at("models", model));
        names.push(format!("model/{model}"));
        Levels(levels, names)
    }

    /// A `policies` member at every level, with the level's name.
    pub(crate) fn named(&self, member: &str) -> Vec<(&str, Option<&'a Value>)> {
        self.1
            .iter()
            .map(String::as_str)
            .zip(self.member(member))
            .collect()
    }

    /// A `policies` member at every level.
    pub(crate) fn member(&self, member: &str) -> Vec<Option<&'a Value>> {
        self.0.iter().map(|p| p.and_then(|p| p.get(member))).collect()
    }

    /// A `policies` member merged over every level.
    pub(crate) fn effective(&self, member: &str) -> Value {
        let mut out = Value::Object(Map::new());
        for level in self.0.iter().flatten().filter_map(|p| p.get(member)) {
            merge(&mut out, level);
        }
        out
    }
}

/// A `policies` member merged over every level that applies to the request.
pub(crate) fn effective(
    snap: &Snapshot,
    member: &str,
    key: &KeyRt,
    model: &str,
    route: Option<&str>,
) -> Value {
    Levels::new(snap, key, model, route).effective(member)
}

fn merge(into: &mut Value, from: &Value) {
    match (into, from) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(k) {
                    Some(slot) if slot.is_object() && v.is_object() => merge(slot, v),
                    _ => {
                        a.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (slot, v) => *slot = v.clone(),
    }
}

fn at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v, |cur, seg| cur.get(*seg))
}

fn duration(v: &Value, path: &[&str], default: Option<Duration>) -> Option<Duration> {
    match at(v, path) {
        Some(Value::String(s)) if s == "none" => None,
        Some(Value::String(s)) => parse_duration(s).or(default),
        _ => default,
    }
}

fn size(v: &Value, path: &[&str], default: u64) -> u64 {
    at(v, path)
        .and_then(Value::as_str)
        .and_then(parse_size)
        .unwrap_or(default)
}

fn int(v: &Value, path: &[&str], default: u64) -> u64 {
    at(v, path).and_then(Value::as_u64).unwrap_or(default)
}

/// Stream settings.
#[derive(Clone, Debug)]
pub(crate) struct StreamPolicy {
    pub(crate) max_event_bytes: usize,
    pub(crate) send_buffer: usize,
    pub(crate) client_write_timeout: Duration,
    pub(crate) max_hold: Duration,
    pub(crate) max_hold_events: usize,
    pub(crate) max_hold_bytes: usize,
    pub(crate) heartbeat: Duration,
    pub(crate) heartbeat_dialects: Vec<String>,
    /// `max_response_bytes`: the decoded upstream bytes a stream may carry.
    pub(crate) max_response_bytes: Option<u64>,
    /// `on_client_disconnect: complete`: a committed stream is read to its end after the client left.
    pub(crate) complete_on_disconnect: bool,
    /// `stream_upstream`: non-streaming requests are streamed from the upstream and gathered.
    pub(crate) stream_upstream: bool,
}

impl StreamPolicy {
    pub(crate) fn new(snap: &Snapshot, policy: &Value) -> StreamPolicy {
        let gw = snap.config.doc.get("streaming").cloned().unwrap_or(Value::Null);
        let to_usize = |n: u64| usize::try_from(n).unwrap_or(usize::MAX);
        let dialects = at(policy, &["heartbeat", "dialects"])
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_else(|| {
                [
                    "openai_chat",
                    "openai_completions",
                    "openai_responses",
                    "anthropic_messages",
                ]
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
            });
        StreamPolicy {
            max_event_bytes: to_usize(size(&gw, &["max_event_bytes"], 16 << 20)),
            send_buffer: to_usize(size(&gw, &["send_buffer"], 1 << 20)),
            client_write_timeout: duration(&gw, &["client_write_timeout"], Some(Duration::from_secs(30)))
                .unwrap_or(Duration::from_secs(30)),
            max_hold: duration(policy, &["commit", "max_hold"], Some(Duration::from_secs(10)))
                .unwrap_or(Duration::from_secs(10)),
            max_hold_events: to_usize(int(policy, &["commit", "max_hold_events"], 64)),
            max_hold_bytes: to_usize(size(policy, &["commit", "max_hold_bytes"], 256 << 10)),
            heartbeat: duration(policy, &["heartbeat", "interval"], Some(Duration::from_secs(15)))
                .unwrap_or(Duration::from_secs(15)),
            heartbeat_dialects: dialects,
            max_response_bytes: at(policy, &["max_response_bytes"])
                .and_then(Value::as_str)
                .and_then(parse_size),
            complete_on_disconnect: at(policy, &["on_client_disconnect"]).and_then(Value::as_str)
                == Some("complete"),
            stream_upstream: at(policy, &["stream_upstream"]).and_then(Value::as_bool) == Some(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn merge_is_field_by_field() {
        let mut a = json!({"retry": {"max_attempts": 5, "tier_retries": 2}});
        merge(&mut a, &json!({"retry": {"max_attempts": 1}}));
        assert_eq!(a, json!({"retry": {"max_attempts": 1, "tier_retries": 2}}));
    }
}

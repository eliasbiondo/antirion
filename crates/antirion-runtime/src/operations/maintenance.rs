//! Maintenance windows (`governance.maintenance_windows`): informational periods the status reports
//! and alert rules may stay silent in.

use antirion_core::UtcMicros;
use serde_json::{Value, json};

/// One configured window; `end` is exclusive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub start: UtcMicros,
    pub end: UtcMicros,
    pub description: Option<String>,
}

impl Window {
    #[must_use]
    pub fn covers(&self, now: UtcMicros) -> bool {
        self.start <= now && now < self.end
    }
}

/// The windows of a document, in their configured order; entries that do not parse are skipped.
#[must_use]
pub fn windows(doc: &Value) -> Vec<Window> {
    doc.pointer("/governance/maintenance_windows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|w| {
            let at = |k: &str| {
                w.get(k)
                    .and_then(Value::as_str)
                    .and_then(|s| UtcMicros::parse_rfc3339(s).ok())
            };
            Some(Window {
                start: at("start")?,
                end: at("end")?,
                description: w.get("description").and_then(Value::as_str).map(str::to_owned),
            })
        })
        .collect()
}

/// The window covering `now` that ends last, when one does.
#[must_use]
pub fn current(doc: &Value, now: UtcMicros) -> Option<Window> {
    windows(doc)
        .into_iter()
        .filter(|w| w.covers(now))
        .max_by_key(|w| w.end)
}

/// The window that starts first after `now`.
#[must_use]
pub fn next(doc: &Value, now: UtcMicros) -> Option<Window> {
    windows(doc)
        .into_iter()
        .filter(|w| w.start > now)
        .min_by_key(|w| w.start)
}

/// What the status reports: the current window, else the next one, else `null`.
#[must_use]
pub fn shown(doc: &Value, now: UtcMicros) -> Value {
    let (window, active) = match current(doc, now) {
        Some(w) => (w, true),
        None => match next(doc, now) {
            Some(w) => (w, false),
            None => return Value::Null,
        },
    };
    json!({
        "start": window.start.to_rfc3339(),
        "end": window.end.to_rfc3339(),
        "description": window.description,
        "active": active,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> UtcMicros {
        UtcMicros::parse_rfc3339(s).unwrap()
    }

    fn doc() -> Value {
        json!({"governance": {"maintenance_windows": [
            {"start": "2026-11-02T02:00:00Z", "end": "2026-11-02T03:00:00Z", "description": "db"},
            {"start": "2026-11-01T02:00:00Z", "end": "2026-11-01T04:00:00Z"},
            {"start": "2026-11-01T03:00:00Z", "end": "2026-11-01T03:30:00Z"},
        ]}})
    }

    #[test]
    fn the_current_window_wins_over_the_next() {
        let d = doc();
        let now = at("2026-11-01T03:15:00Z");
        assert_eq!(current(&d, now).unwrap().end, at("2026-11-01T04:00:00Z"));
        let shown = shown(&d, now);
        assert_eq!(shown["active"], true);
        assert_eq!(shown["end"], "2026-11-01T04:00:00Z");
    }

    #[test]
    fn the_next_window_is_the_earliest_start_ahead() {
        let d = doc();
        let shown = shown(&d, at("2026-11-01T05:00:00Z"));
        assert_eq!(shown["active"], false);
        assert_eq!(shown["start"], "2026-11-02T02:00:00Z");
        assert_eq!(shown["description"], "db");
        assert!(super::shown(&d, at("2026-12-01T00:00:00Z")).is_null());
        assert!(!windows(&d)[0].covers(at("2026-11-02T03:00:00Z")));
    }
}

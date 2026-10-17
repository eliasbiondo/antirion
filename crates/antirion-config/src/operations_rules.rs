//! Rules of the operations settings: every maintenance window ends after it starts.

use serde_json::Value;

use crate::{ConfigError, DetailCode, DocPath};

pub(crate) fn check(doc: &Value, errors: &mut Vec<ConfigError>) {
    let windows = doc
        .pointer("/governance/maintenance_windows")
        .and_then(Value::as_array);
    for (i, w) in windows.into_iter().flatten().enumerate() {
        let at = |k: &str| {
            w.get(k)
                .and_then(Value::as_str)
                .and_then(|s| antirion_core::UtcMicros::parse_rfc3339(s).ok())
        };
        if let (Some(start), Some(end)) = (at("start"), at("end"))
            && end <= start
        {
            errors.push(ConfigError::new(
                &DocPath::root()
                    .key("governance")
                    .key("maintenance_windows")
                    .index(i)
                    .key("end"),
                DetailCode::OutOfRange,
                "a maintenance window must end after it starts",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_window_must_end_after_it_starts() {
        let doc = json!({"governance": {"maintenance_windows": [
            {"start": "2026-11-02T02:00:00Z", "end": "2026-11-02T03:00:00Z"},
            {"start": "2026-11-02T03:00:00Z", "end": "2026-11-02T03:00:00Z"},
            {"start": "2026-11-02T03:00:00+01:00", "end": "2026-11-02T02:30:00Z"},
        ]}});
        let mut errors = Vec::new();
        check(&doc, &mut errors);
        let paths: Vec<&str> = errors.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["governance.maintenance_windows[1].end"]);
    }
}

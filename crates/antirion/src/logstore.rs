//! `store.logs`: the request log on ClickHouse instead of the primary store.

use std::time::Duration;

use antirion_config::scalar::parse_duration;
use antirion_store::chlogs::{ClickHouseLogs, ClickHouseSettings};
use anyhow::anyhow;
use serde_json::Value;

use crate::startup::resolved_string;

/// The ClickHouse log store the startup document configures, if it does.
pub(crate) fn clickhouse(doc: &Value) -> anyhow::Result<Option<ClickHouseLogs>> {
    if doc.pointer("/store/logs/backend").and_then(Value::as_str) != Some("clickhouse") {
        return Ok(None);
    }
    let url = resolved_string(doc, "/store/logs/clickhouse/url")?
        .ok_or_else(|| anyhow!("store.logs.backend clickhouse needs store.logs.clickhouse.url"))?;
    let block = doc.pointer("/store/logs/clickhouse");
    let text = |k: &str| block.and_then(|b| b.get(k)).and_then(Value::as_str);
    let duration = |k: &str, d: Duration| text(k).and_then(parse_duration).unwrap_or(d);
    let database = text("database").unwrap_or("antirion");
    if !database.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(anyhow!(
            "store.logs.clickhouse.database must be letters, digits and underscores"
        ));
    }
    let settings = ClickHouseSettings {
        url: antirion_core::Secret::new(url),
        database: database.to_owned(),
        batch_size: block
            .and_then(|b| b.get("batch_size"))
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(10_000),
        flush_interval: duration("flush_interval", Duration::from_secs(1)),
        insert_timeout: duration("insert_timeout", Duration::from_secs(10)),
    };
    Ok(Some(ClickHouseLogs::new(settings)?))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_the_clickhouse_backend_builds_a_store() {
        assert!(clickhouse(&json!({})).unwrap().is_none());
        let doc = json!({"store": {"logs": {"backend": "clickhouse",
            "clickhouse": {"url": "http://ch:8123", "batch_size": 5, "flush_interval": "200ms"}}}});
        let s = clickhouse(&doc).unwrap().unwrap();
        assert_eq!(s.settings().batch_size, 5);
        assert_eq!(s.settings().flush_interval, Duration::from_millis(200));
        let bad = json!({"store": {"logs": {"backend": "clickhouse",
            "clickhouse": {"url": "http://ch:8123", "database": "x;drop"}}}});
        assert!(clickhouse(&bad).is_err());
    }
}

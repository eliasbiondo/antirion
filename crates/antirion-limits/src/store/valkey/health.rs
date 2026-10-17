//! The cluster's shared health state on the counter store: one hash of records, each written only
//! when it is newer than the one it replaces, and a version that changes with every write, so a
//! node reads the hash only when something changed.

use redis::Value;

use super::{ValkeyCounters, items, text};
use crate::store::StoreError;

/// Writes each (field, stamp, value) whose stamp is newer than the stored value's, which starts
/// with its own stamp, and bumps the version when anything changed.
const PUT: &str = r"
local changed = 0
for i = 1, #ARGV, 3 do
  local current = redis.call('HGET', KEYS[1], ARGV[i])
  local stamp = -1
  if current then stamp = tonumber(string.match(current, '^(%-?%d+)')) or -1 end
  if tonumber(ARGV[i + 1]) > stamp then
    redis.call('HSET', KEYS[1], ARGV[i], ARGV[i + 2])
    changed = 1
  end
end
if changed == 1 then redis.call('INCR', KEYS[2]) end
return changed
";

impl ValkeyCounters {
    fn health_keys(&self) -> (String, String) {
        (
            format!("{}{{h}}health", self.prefix),
            format!("{}{{h}}health:v", self.prefix),
        )
    }

    /// Stores health records (field, stamp, value) that are newer than the stored ones.
    pub async fn health_put(&self, records: &[(String, i64, String)]) -> Result<(), StoreError> {
        if records.is_empty() {
            return Ok(());
        }
        let (hash, version) = self.health_keys();
        let mut cmd = redis::cmd("EVAL");
        cmd.arg(PUT).arg(2).arg(hash).arg(version);
        for (field, stamp, value) in records {
            cmd.arg(field).arg(stamp.to_string()).arg(value);
        }
        self.run(&cmd).await.map(|_| ())
    }

    /// The version of the shared health state: it changes whenever a record does.
    pub async fn health_version(&self) -> Result<i64, StoreError> {
        let (_, version) = self.health_keys();
        let mut get = redis::cmd("GET");
        get.arg(version);
        Ok(text(&self.run(&get).await?)
            .and_then(|t| t.parse().ok())
            .unwrap_or(0))
    }

    /// Every shared health record: (field, value).
    pub async fn health_records(&self) -> Result<Vec<(String, String)>, StoreError> {
        let (hash, _) = self.health_keys();
        let mut all = redis::cmd("HGETALL");
        all.arg(hash);
        let reply = self.run(&all).await?;
        let flat: Vec<String> = match &reply {
            Value::Map(pairs) => pairs
                .iter()
                .flat_map(|(k, v)| [text(k), text(v)])
                .flatten()
                .collect(),
            other => items(other).iter().filter_map(text).collect(),
        };
        Ok(flat
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[f, v]| (f.clone(), v.clone()))
            .collect())
    }
}

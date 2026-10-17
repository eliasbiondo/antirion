//! The entry index on the server: what the scripts of `scripts` are called with and how their
//! answers are read.

use redis::Value;
use serde::{Deserialize, Serialize};

use super::{SCRIPTS, ValkeyStore, err};
use crate::CacheError;
use crate::entry::Entry;
use crate::key::sha256_hex;
use crate::listing::{self, ListQuery, Listed, Listing, Position, SortKey, Totals};
use crate::meta::EntryMeta;

/// What the index keeps of an entry besides its numbers.
#[derive(Serialize, Deserialize)]
struct Stored {
    meta: EntryMeta,
    deployment: String,
    provider: String,
}

/// The short hash a dimension value or cap scope is keyed by.
fn short(s: &str) -> String {
    let mut h = sha256_hex(s.as_bytes());
    h.truncate(16);
    h
}

fn sort_index(sort: SortKey) -> u8 {
    match sort {
        SortKey::LastHit => 0,
        SortKey::Created => 1,
        SortKey::Expires => 2,
        SortKey::Hits => 3,
        SortKey::Size => 4,
    }
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::BulkString(b) => String::from_utf8(b.clone()).ok(),
        Value::SimpleString(s) => Some(s.clone()),
        Value::Int(n) => Some(n.to_string()),
        _ => None,
    }
}

fn int(v: &Value) -> i64 {
    match v {
        Value::Int(n) => *n,
        other => text(other)
            .and_then(|t| t.parse::<f64>().ok())
            .map_or(0, |f| f as i64),
    }
}

fn items(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) | Value::Set(a) => a,
        _ => Vec::new(),
    }
}

/// Reads rows of seven values: key, meta, last use, created, expires, hits, size.
fn rows(values: &[Value]) -> Vec<Listed> {
    values
        .as_chunks::<7>()
        .0
        .iter()
        .filter_map(|[key, meta, last, created, expires, hits, size]| {
            let stored: Stored = serde_json::from_str(&text(meta)?).ok()?;
            let last = int(last);
            Some(Listed {
                key: text(key)?,
                meta: stored.meta,
                deployment: stored.deployment,
                provider: stored.provider,
                size_bytes: u64::try_from(int(size)).unwrap_or(0),
                hits: u64::try_from(int(hits)).unwrap_or(0),
                created_ms: int(created),
                expires_ms: int(expires),
                last_hit_ms: (last >= 0).then_some(last),
            })
        })
        .collect()
}

impl ValkeyStore {
    fn pairs(v: Value) -> Vec<String> {
        items(v).iter().filter_map(text).collect()
    }

    /// Indexes `entry` (written under scope digest `digest`); the `digest:key` of entries a cap
    /// evicted.
    pub(super) async fn index_put(
        &self,
        digest: &str,
        key: &str,
        entry: &Entry,
        now_ms: i64,
    ) -> Result<Vec<String>, CacheError> {
        let meta = entry.meta.clone().unwrap_or_default();
        let dims: Vec<String> = meta.dims(&entry.deployment).iter().map(|d| short(d)).collect();
        let caps: Vec<(String, u64)> = meta.caps.iter().map(|c| (short(&c.scope), c.max)).collect();
        let x = format!(
            "{digest}|{}|{}",
            dims.join(" "),
            caps.iter().map(|(h, _)| h.as_str()).collect::<Vec<_>>().join(" ")
        );
        let stored = Stored {
            meta,
            deployment: entry.deployment.clone(),
            provider: entry.provider.clone(),
        };
        let json = serde_json::to_string(&stored).map_err(|_| CacheError::Protocol)?;
        let mut inv = SCRIPTS.put.call(&self.base);
        inv.arg(key)
            .arg(json)
            .arg(x)
            .arg(entry.written_at_ms)
            .arg(entry.expires_at_ms())
            .arg(entry.weight())
            .arg(listing::last_use_value(entry.written_at_ms, None))
            .arg(now_ms);
        for (h, max) in &caps {
            inv.arg(h).arg(*max);
        }
        let mut conn = self.conn.clone();
        let v: Value = inv.query_async(&mut conn).await.map_err(|e| err(&e))?;
        Ok(Self::pairs(v))
    }

    /// Counts a hit of the entry under `key`.
    pub async fn touch(&self, key: &str, now_ms: i64) -> Result<(), CacheError> {
        let mut conn = self.conn.clone();
        SCRIPTS
            .touch
            .call(&self.base)
            .arg(key)
            .arg(now_ms)
            .query_async::<Value>(&mut conn)
            .await
            .map(|_| ())
            .map_err(|e| err(&e))
    }

    /// Takes `key` out of the index; the scope digest it was written under, when it was indexed.
    pub(super) async fn index_remove(&self, key: &str) -> Result<Option<String>, CacheError> {
        let mut conn = self.conn.clone();
        let v: Value = SCRIPTS
            .remove
            .call(&self.base)
            .arg(key)
            .query_async(&mut conn)
            .await
            .map_err(|e| err(&e))?;
        Ok(text(&v))
    }

    /// Deletes the entries carrying `dim` (of scope digest `digest`, when given) written at or
    /// before `written_le`; how many.
    pub async fn drop_dim(
        &self,
        dim: &str,
        digest: Option<&str>,
        written_le: i64,
    ) -> Result<u64, CacheError> {
        let mut conn = self.conn.clone();
        let v: Value = SCRIPTS
            .drop_dim
            .call(&self.base)
            .arg(short(dim))
            .arg(digest.unwrap_or_default())
            .arg(written_le)
            .query_async(&mut conn)
            .await
            .map_err(|e| err(&e))?;
        let pairs = Self::pairs(v);
        self.delete_pairs(&pairs).await?;
        Ok(u64::try_from(pairs.len()).unwrap_or(u64::MAX))
    }

    /// Deletes the entries written at or before `written_le`; how many.
    pub async fn drop_older(&self, written_le: i64) -> Result<u64, CacheError> {
        let mut conn = self.conn.clone();
        let v: Value = SCRIPTS
            .drop_older
            .call(&self.base)
            .arg(written_le)
            .query_async(&mut conn)
            .await
            .map_err(|e| err(&e))?;
        let pairs = Self::pairs(v);
        self.delete_pairs(&pairs).await?;
        Ok(u64::try_from(pairs.len()).unwrap_or(u64::MAX))
    }

    /// Live entries and their bytes.
    pub async fn size(&self, now_ms: i64) -> Result<(u64, u64), CacheError> {
        let mut conn = self.conn.clone();
        let v: Value = SCRIPTS
            .size
            .call(&self.base)
            .arg(now_ms)
            .query_async(&mut conn)
            .await
            .map_err(|e| err(&e))?;
        let n = items(v);
        let at = |i: usize| n.get(i).map_or(0, |v| u64::try_from(int(v)).unwrap_or(0));
        Ok((at(0), at(1)))
    }

    /// How many live entries carry `dim`.
    pub async fn count(&self, dim: &str, now_ms: i64) -> Result<u64, CacheError> {
        let mut conn = self.conn.clone();
        let v: Value = SCRIPTS
            .count
            .call(&self.base)
            .arg(now_ms)
            .arg(short(dim))
            .query_async(&mut conn)
            .await
            .map_err(|e| err(&e))?;
        Ok(u64::try_from(int(&v)).unwrap_or(0))
    }

    /// A page of live entries.
    pub async fn list(&self, q: &ListQuery) -> Result<Listing, CacheError> {
        let mut inv = SCRIPTS.list.call(&self.base);
        let after = q.after.as_ref();
        inv.arg(q.now_ms)
            .arg(sort_index(q.sort))
            .arg(u8::from(q.descending))
            .arg(u8::from(after.is_some()))
            .arg(after.map_or(0, |p| p.value))
            .arg(after.map_or("", |p| p.key.as_str()))
            .arg(q.limit)
            .arg(q.prefix.as_deref().unwrap_or_default())
            .arg(u8::from(q.totals))
            .arg(q.all_of.len());
        for d in &q.all_of {
            inv.arg(short(d));
        }
        match &q.any_of {
            None => {
                inv.arg(-1);
            }
            Some(any) => {
                inv.arg(any.len());
                for d in any {
                    inv.arg(short(d));
                }
            }
        }
        let mut conn = self.conn.clone();
        let v: Value = inv.query_async(&mut conn).await.map_err(|e| err(&e))?;
        let all = items(v);
        let head = |i: usize| all.get(i).map_or(0, int);
        let more = head(0) == 1;
        let items = rows(all.get(4..).unwrap_or_default());
        let next = more.then(|| items.last()).flatten().map(|i| Position {
            value: i.sort_value(q.sort),
            key: i.key.clone(),
        });
        let as_u = |n: i64| u64::try_from(n).unwrap_or(0);
        Ok(Listing {
            next,
            totals: q.totals.then(|| Totals {
                count: as_u(head(1)),
                bytes: as_u(head(2)),
                hits: as_u(head(3)),
            }),
            items,
        })
    }

    /// The live entry under `key` as a listing reports it, and the entry when it is still stored.
    pub async fn read(&self, key: &str, now_ms: i64) -> Result<Option<(Listed, Option<Entry>)>, CacheError> {
        let mut conn = self.conn.clone();
        let v: Value = SCRIPTS
            .read
            .call(&self.base)
            .arg(now_ms)
            .arg(key)
            .query_async(&mut conn)
            .await
            .map_err(|e| err(&e))?;
        let Some(item) = rows(&items(v)).into_iter().next() else {
            return Ok(None);
        };
        let entry = self.get(&item.meta.digest, key).await?;
        Ok(Some((item, entry)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::{Cap, Mode};

    fn entry(at: i64, model: &str, caps: &[(&str, u64)]) -> Entry {
        Entry {
            written_at_ms: at,
            ttl_ms: 600_000,
            content_type: "application/json".into(),
            body: Some("{}".into()),
            events: None,
            provider: "p".into(),
            deployment: "d".into(),
            model: "up".into(),
            upstream_status: 200,
            upstream_dialect: None,
            usage: None,
            meta: Some(EntryMeta {
                mode: Mode::Exact,
                digest: "0123456789abcdef".into(),
                model: model.into(),
                project: "p".into(),
                caps: caps
                    .iter()
                    .map(|(s, m)| Cap {
                        scope: (*s).into(),
                        max: *m,
                    })
                    .collect(),
                ..EntryMeta::default()
            }),
        }
    }

    #[test]
    fn rows_read_scores() {
        let b = |s: &str| Value::BulkString(s.as_bytes().to_vec());
        let meta = serde_json::to_string(&Stored {
            meta: EntryMeta::default(),
            deployment: "d".into(),
            provider: "p".into(),
        })
        .unwrap();
        let got = rows(&[
            b("k"),
            b(&meta),
            b("-3998300000000000"),
            b("5"),
            b("9"),
            b("2"),
            b("40"),
        ]);
        assert_eq!(got.len(), 1);
        let r = &got[0];
        assert_eq!((r.created_ms, r.expires_ms, r.hits, r.size_bytes), (5, 9, 2, 40));
        assert_eq!(r.last_hit_ms, None);
    }

    /// Runs against a server when `ANTIRION_TEST_VALKEY_URL` is set.
    #[tokio::test]
    async fn the_index_lists_caps_and_drops() {
        let Ok(url) = std::env::var("ANTIRION_TEST_VALKEY_URL") else {
            return;
        };
        let s = ValkeyStore::new(&url, "antirion:cacheindex:", false).unwrap();
        s.clear().await.unwrap();
        let k = |c: char| c.to_string().repeat(64);
        let now = 1_000;
        s.put(
            "0123456789abcdef",
            &k('a'),
            &entry(now, "m", &[("model/m", 2)]),
            600_000,
            now,
        )
        .await
        .unwrap();
        s.put(
            "0123456789abcdef",
            &k('b'),
            &entry(now + 1, "m", &[("model/m", 2)]),
            600_000,
            now + 1,
        )
        .await
        .unwrap();
        s.put(
            "0123456789abcdef",
            &k('c'),
            &entry(now + 2, "n", &[]),
            600_000,
            now + 2,
        )
        .await
        .unwrap();
        s.touch(&k('a'), now + 3).await.unwrap();
        s.put(
            "0123456789abcdef",
            &k('d'),
            &entry(now + 4, "m", &[("model/m", 2)]),
            600_000,
            now + 4,
        )
        .await
        .unwrap();
        assert!(
            s.get("0123456789abcdef", &k('b')).await.unwrap().is_none(),
            "the cap evicted b"
        );
        let q = ListQuery {
            descending: true,
            limit: 2,
            totals: true,
            now_ms: now + 5,
            ..ListQuery::default()
        };
        let page = s.list(&q).await.unwrap();
        let keys: Vec<String> = page.items.iter().map(|i| i.key.clone()).collect();
        assert_eq!(keys, [k('a'), k('d')]);
        assert_eq!(page.totals.map(|t| (t.count, t.hits)), Some((3, 1)));
        let rest = s
            .list(&ListQuery {
                after: page.next,
                ..q.clone()
            })
            .await
            .unwrap();
        assert_eq!(rest.items.len(), 1);
        let only_m = s
            .list(&ListQuery {
                all_of: vec!["model/m".into()],
                ..q.clone()
            })
            .await
            .unwrap();
        assert_eq!(only_m.totals.map(|t| t.count), Some(2));
        assert_eq!(s.count("model/m", now + 5).await.unwrap(), 2);
        assert_eq!(s.drop_dim("model/n", None, now + 5).await.unwrap(), 1);
        assert_eq!(s.drop_older(now).await.unwrap(), 1);
        assert_eq!(s.size(now + 5).await.unwrap().0, 1);
        let (item, stored) = s.read(&k('d'), now + 5).await.unwrap().unwrap();
        assert_eq!(item.meta.model, "m");
        assert!(stored.is_some());
        assert!(s.delete(&k('d')).await.unwrap());
        assert_eq!(s.size(now + 5).await.unwrap(), (0, 0));
        s.clear().await.unwrap();
    }
}

//! Removing what a deleted organization leaves in the store, and ending every operator session at
//! once. Each statement names the organization, its teams, projects and keys explicitly, so rows of
//! any other tenant are never matched; the audit log is kept.

use rusqlite::params;

use crate::{Store, StoreError};

/// The tenancy subtree whose rows an erasure removes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tenant {
    pub organization: String,
    pub teams: Vec<String>,
    pub projects: Vec<String>,
    /// Key names.
    pub keys: Vec<String>,
    /// The keys' incarnation ids, in hex.
    pub key_uids: Vec<String>,
}

impl Tenant {
    /// The role-binding scopes inside the subtree.
    #[must_use]
    pub fn scopes(&self) -> Vec<String> {
        std::iter::once(format!("organization/{}", self.organization))
            .chain(self.teams.iter().map(|t| format!("team/{t}")))
            .chain(self.projects.iter().map(|p| format!("project/{p}")))
            .collect()
    }
}

/// Budget rows owned by a level of the subtree; `?1` is the organization and `?2` … `?4` JSON
/// arrays of team, project and key names.
const OWNED_LITE: &str = "((level = 'organization' AND owner = ?1)
    OR (level = 'team' AND owner IN (SELECT value FROM json_each(?2)))
    OR (level = 'project' AND owner IN (SELECT value FROM json_each(?3)))
    OR (level = 'key' AND owner IN (SELECT value FROM json_each(?4))))";

pub(crate) const OWNED_PG: &str = "((level = 'organization' AND owner = $1)
    OR (level = 'team' AND owner = ANY($2))
    OR (level = 'project' AND owner = ANY($3))
    OR (level = 'key' AND owner = ANY($4)))";

fn json_list(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".to_owned())
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

impl Store {
    /// Ends every operator session; returns how many there were.
    pub async fn delete_all_sessions(&self) -> Result<u64, StoreError> {
        match self {
            Store::Sqlite(s) => {
                s.call(|c| Ok(count(c.execute("DELETE FROM admin_sessions", [])?)))
                    .await
            }
            Store::Postgres(p) => p.execute("DELETE FROM admin_sessions", &[]).await,
        }
    }

    /// Deletes the requests of `kind` created before `before_us`; returns how many went.
    pub async fn delete_requests_before(&self, kind: &str, before_us: i64) -> Result<u64, StoreError> {
        let kind = kind.to_owned();
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(count(c.execute(
                        "DELETE FROM pending_requests WHERE kind = ?1 AND created_us < ?2",
                        params![kind, before_us],
                    )?))
                })
                .await
            }
            Store::Postgres(p) => {
                p.execute(
                    "DELETE FROM pending_requests WHERE kind = $1 AND created_us < $2",
                    &[&kind, &before_us],
                )
                .await
            }
        }
    }

    /// Deletes one request; whether it existed.
    pub async fn delete_request(&self, kind: &str, id: &str) -> Result<bool, StoreError> {
        let (kind, id) = (kind.to_owned(), id.to_owned());
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    Ok(c.execute(
                        "DELETE FROM pending_requests WHERE kind = ?1 AND id = ?2",
                        params![kind, id],
                    )? > 0)
                })
                .await
            }
            Store::Postgres(p) => p
                .execute(
                    "DELETE FROM pending_requests WHERE kind = $1 AND id = $2",
                    &[&kind, &id],
                )
                .await
                .map(|n| n > 0),
        }
    }

    /// Deletes the ledger lines of `organization` admitted before `before_us`, and their charges: what
    /// a node still wrote of a tenant after its erasure.
    pub async fn erase_ledger(&self, organization: &str, before_us: i64) -> Result<u64, StoreError> {
        let org = organization.to_owned();
        match self {
            Store::Sqlite(s) => {
                s.call(move |c| {
                    let tx = c.transaction()?;
                    let mut n = tx.execute(
                        "DELETE FROM ledger_charges WHERE EXISTS (SELECT 1 FROM ledger_lines l
                            WHERE l.request_id = ledger_charges.request_id AND l.line = ledger_charges.line
                              AND l.admitted_us < ?2 AND json_valid(l.body)
                              AND json_extract(l.body, '$.organization') = ?1)",
                        params![org, before_us],
                    )?;
                    n += tx.execute(
                        "DELETE FROM ledger_lines WHERE admitted_us < ?2 AND json_valid(body)
                           AND json_extract(body, '$.organization') = ?1",
                        params![org, before_us],
                    )?;
                    tx.commit()?;
                    Ok(count(n))
                })
                .await
            }
            Store::Postgres(p) => p.erase_ledger(&org, before_us).await,
        }
    }

    /// Deletes the rows of `t` outside the request log: ledger lines and their charges, budget
    /// grants, overage requests, events and tallies of its budgets, key use times, anomaly flags and
    /// history, upstream objects and stored responses of its projects, admin-token role bindings
    /// scoped inside it, sessions signed in to the organization, request-log rollups and safety
    /// events. Returns how many rows went.
    pub async fn erase_tenant(&self, t: &Tenant) -> Result<u64, StoreError> {
        match self {
            Store::Sqlite(s) => {
                let state = t.clone();
                let mut n = s
                    .call(move |c| {
                        let tx = c.transaction()?;
                        let n = erase_state_lite(&tx, &state)?;
                        tx.commit()?;
                        Ok(n)
                    })
                    .await?;
                let org = t.organization.clone();
                n += s
                    .logs()
                    .call(move |c| {
                        let tx = c.transaction()?;
                        let mut n = tx.execute("DELETE FROM log_rollups WHERE organization = ?1", [&org])?;
                        n += tx.execute("DELETE FROM safety_events WHERE organization = ?1", [&org])?;
                        tx.commit()?;
                        Ok(count(n))
                    })
                    .await?;
                Ok(n)
            }
            Store::Postgres(p) => p.erase_tenant(t).await,
        }
    }
}

fn erase_state_lite(tx: &rusqlite::Transaction<'_>, t: &Tenant) -> Result<u64, StoreError> {
    let (teams, projects, keys, uids, scopes) = (
        json_list(&t.teams),
        json_list(&t.projects),
        json_list(&t.keys),
        json_list(&t.key_uids),
        json_list(&t.scopes()),
    );
    let org = t.organization.as_str();
    let owned = params![org, teams, projects, keys];
    let mut n = 0;
    for table in ["budget_grants", "overage_requests", "budget_events"] {
        n += tx.execute(
            &format!(
                "DELETE FROM budget_tallies WHERE identity IN (SELECT identity FROM {table} WHERE {OWNED_LITE})"
            ),
            owned,
        )?;
    }
    for table in ["budget_grants", "overage_requests", "budget_events"] {
        n += tx.execute(&format!("DELETE FROM {table} WHERE {OWNED_LITE}"), owned)?;
    }
    n += tx.execute(
        "DELETE FROM ledger_charges WHERE EXISTS (SELECT 1 FROM ledger_lines l
            WHERE l.request_id = ledger_charges.request_id AND l.line = ledger_charges.line
              AND json_valid(l.body) AND json_extract(l.body, '$.organization') = ?1)",
        [org],
    )?;
    n += tx.execute(
        "DELETE FROM ledger_lines WHERE json_valid(body) AND json_extract(body, '$.organization') = ?1",
        [org],
    )?;
    n += tx.execute(
        "DELETE FROM use_marks WHERE kind = 'key' AND id IN (SELECT value FROM json_each(?1))",
        [&uids],
    )?;
    n += tx.execute("DELETE FROM key_anomalies WHERE organization = ?1", [org])?;
    for table in ["key_seen", "key_hours"] {
        n += tx.execute(
            &format!("DELETE FROM {table} WHERE key_uid IN (SELECT value FROM json_each(?1))"),
            [&uids],
        )?;
    }
    n += tx.execute(
        "DELETE FROM upstream_objects WHERE project IN (SELECT value FROM json_each(?1))",
        [&projects],
    )?;
    n += tx.execute(
        "DELETE FROM token_bindings WHERE scope IN (SELECT value FROM json_each(?1))",
        [&scopes],
    )?;
    n += tx.execute(
        "DELETE FROM admin_sessions WHERE json_valid(facts) AND json_extract(facts, '$.organization') = ?1",
        [org],
    )?;
    Ok(count(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "antirion-store-erasure-{:x}",
            antirion_core::ResourceUid::random().0
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn tenant() -> Tenant {
        Tenant {
            organization: "acme".into(),
            teams: vec!["red".into()],
            projects: vec!["web".into()],
            keys: vec!["k1".into()],
            key_uids: vec!["00aa".into()],
        }
    }

    #[test]
    fn scopes_cover_every_level() {
        assert_eq!(
            tenant().scopes(),
            vec!["organization/acme", "team/red", "project/web"]
        );
    }

    #[tokio::test]
    async fn only_the_tenants_rows_go() {
        let dir = temp_dir();
        let store = Store::open_embedded(&dir).unwrap();
        let Store::Sqlite(s) = &store else { unreachable!() };
        s.call(|c| {
            for (org, key, uid, rid) in [("acme", "k1", "00aa", "r1"), ("other", "k2", "00bb", "r2")] {
                c.execute(
                    "INSERT INTO ledger_lines (request_id, line, admitted_us, body) VALUES (?1, 0, 1, ?2)",
                    params![rid, format!(r#"{{"organization":"{org}","budgets":[]}}"#)],
                )?;
                c.execute(
                    "INSERT INTO ledger_charges (counter, admitted_us, request_id, line, amount) VALUES (?1, 1, ?2, 0, 5)",
                    params![format!("c-{org}"), rid],
                )?;
                c.execute("INSERT INTO use_marks (kind, id, at_us) VALUES ('key', ?1, 1)", [uid])?;
                c.execute(
                    "INSERT INTO budget_grants (id, identity, level, owner, name, amount, created_us, expires_us, reason, granted_by)
                     VALUES (?1, ?2, 'key', ?3, 'b', '1', 0, 1, 'r', 'u')",
                    params![format!("g-{org}"), format!("i-{org}"), key],
                )?;
                c.execute(
                    "INSERT INTO budget_tallies (identity, period_start_us, node, refused, downgraded) VALUES (?1, 0, 'n', 1, 0)",
                    [format!("i-{org}")],
                )?;
                c.execute(
                    "INSERT INTO token_bindings (id, kid, role, scope, created_at) VALUES (?1, 'kid', 'viewer', ?2, 't')",
                    params![format!("b-{org}"), format!("organization/{org}")],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
        let n = store.erase_tenant(&tenant()).await.unwrap();
        assert_eq!(n, 6);
        let left: Vec<i64> = s
            .call(|c| {
                let mut out = Vec::new();
                for table in [
                    "ledger_lines",
                    "ledger_charges",
                    "use_marks",
                    "budget_grants",
                    "budget_tallies",
                    "token_bindings",
                ] {
                    out.push(c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?);
                }
                Ok(out)
            })
            .await
            .unwrap();
        assert_eq!(left, vec![1, 1, 1, 1, 1, 1]);
        let other: String = s
            .call(|c| Ok(c.query_row("SELECT owner FROM budget_grants", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(other, "k2");
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn every_session_ends() {
        let dir = temp_dir();
        let store = Store::open_embedded(&dir).unwrap();
        for id in ["a", "b"] {
            store
                .put_session(crate::SessionRecord {
                    id_digest: id.into(),
                    kid: "bootstrap".into(),
                    created_us: 0,
                    last_used_us: 0,
                    user: None,
                    facts: "{}".into(),
                })
                .await
                .unwrap();
        }
        assert_eq!(store.delete_all_sessions().await.unwrap(), 2);
        assert_eq!(store.delete_all_sessions().await.unwrap(), 0);
        std::fs::remove_dir_all(dir).ok();
    }
}

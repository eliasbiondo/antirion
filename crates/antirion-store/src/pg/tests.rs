//! Run when `ANTIRION_TEST_POSTGRES_URL` names a PostgreSQL server the tests may create databases on:
//!
//! ```text
//! ANTIRION_TEST_POSTGRES_URL=postgres://antirion:antirion@127.0.0.1:55432/antirion cargo test -p antirion-store
//! ```

use std::time::Duration;

use super::*;
use crate::{GenerationRecord, LedgerRow, StoredConfig};

/// A fresh database on the test server, and settings for it.
async fn fresh(node: &str) -> Option<PgSettings> {
    let base = std::env::var("ANTIRION_TEST_POSTGRES_URL").ok()?;
    let name = format!("t_{:x}", antirion_core::ResourceUid::random().0);
    let (client, conn) = tokio_postgres::connect(&base, tokio_postgres::NoTls).await.ok()?;
    let mut set = tokio::task::JoinSet::new();
    set.spawn(async move {
        let _ = conn.await;
    });
    client
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .await
        .ok()?;
    let (prefix, _) = base.rsplit_once('/')?;
    Some(settings(&format!("{prefix}/{name}"), node))
}

fn settings(url: &str, node: &str) -> PgSettings {
    PgSettings {
        url: url.to_owned(),
        pool_size: 2,
        connect_timeout: Duration::from_secs(5),
        statement_timeout: Duration::from_secs(5),
        migrate: Migrate::Auto,
        migrate_timeout: Duration::from_secs(30),
        binary_version: "0.1.0".into(),
        node_id: node.into(),
    }
}

fn config(generation: u64) -> (StoredConfig, GenerationRecord) {
    (
        StoredConfig {
            generation,
            document: "{}".into(),
            metadata: "{}".into(),
        },
        GenerationRecord {
            generation,
            created_at: "t".into(),
            actor: "a".into(),
            source: "api".into(),
            summary: String::new(),
        },
    )
}

#[tokio::test]
async fn concurrent_opens_migrate_once_and_commits_never_skip_a_generation() {
    let Some(s) = fresh("a").await else { return };
    let sb = settings(&s.url, "b");
    let (a, b) = tokio::join!(PgStore::open(&s), PgStore::open(&sb));
    let (a, b) = (a.unwrap(), b.unwrap());
    let (c1, h1) = config(1);
    a.commit_config(c1, h1, None).await.unwrap();
    let (c1b, h1b) = config(1);
    assert!(matches!(
        b.commit_config(c1b, h1b, None).await,
        Err(StoreError::Conflict)
    ));
    let (c2, h2) = config(2);
    b.commit_config(c2, h2, None).await.unwrap();
    assert_eq!(a.head().await.unwrap(), 2);
    let old = PgSettings {
        binary_version: "0.0.1".into(),
        ..s.clone()
    };
    assert!(matches!(
        PgStore::open(&old).await,
        Err(StoreError::SchemaTooNew { .. })
    ));
}

#[tokio::test]
async fn leases_ledger_lines_and_notifications() {
    let Some(s) = fresh("a").await else { return };
    let a = PgStore::open(&s).await.unwrap();
    let b = PgStore::open(&settings(&s.url, "b")).await.unwrap();
    let ttl = Duration::from_secs(15);
    assert!(a.lease("job", "a", ttl).await.unwrap());
    assert!(!b.lease("job", "b", ttl).await.unwrap());
    assert!(a.lease("job", "a", ttl).await.unwrap());
    let row = |id: &str| LedgerRow {
        request_id: id.into(),
        line: 0,
        admitted_us: 1,
        body: "{}".into(),
        charges: None,
    };
    a.append_ledger(vec![row("r1"), row("r2")]).await.unwrap();
    b.append_ledger(vec![row("r2"), row("r3")]).await.unwrap();
    let (rows, newest) = b.ledger_since(0, 100).await.unwrap();
    assert_eq!(rows.len(), 3);
    assert!(b.ledger_since(newest, 100).await.unwrap().0.is_empty());
    let mut listener = b.listen().await.unwrap();
    let (c1, h1) = config(1);
    a.commit_config(c1, h1, None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), listener.generations.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*listener.generations.borrow(), 1);
}

#[tokio::test]
async fn a_store_opened_offline_checks_its_schema_before_it_is_used() {
    let Some(s) = fresh("a").await else { return };
    let store = PgStore::offline(&s).unwrap();
    assert!(store.is_down());
    assert!(matches!(store.head().await, Err(StoreError::Unavailable)));
    let mut probes = 0;
    while !store.probe().await {
        probes += 1;
        assert!(probes < PROBES_TO_RECOVER, "recovered after {probes} probes");
    }
    assert_eq!(store.head().await.unwrap(), 0);
}

/// Runs when `ANTIRION_TEST_POSTGRES_TLS_URL` names a server that accepts TLS, with the `sslmode`
/// (and `sslrootcert`) to use.
#[tokio::test]
async fn connections_use_tls_when_the_url_asks() {
    let Ok(url) = std::env::var("ANTIRION_TEST_POSTGRES_TLS_URL") else {
        return;
    };
    let store = PgStore::open(&settings(&url, "a")).await.unwrap();
    let mut lease = store.pool.get().await.unwrap();
    let row = lease
        .client()
        .unwrap()
        .query_one("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()", &[])
        .await
        .unwrap();
    assert!(row.get::<_, bool>(0));
}

#[tokio::test]
async fn identity_records_sessions_and_token_use() {
    let Some(s) = fresh("identity").await else {
        return;
    };
    let store = crate::Store::open_postgres(&s).await.unwrap();
    let rec = |kind: &str, id: &str, user: &str| crate::IdentityRecord {
        kind: kind.into(),
        id: id.into(),
        user: user.into(),
        expires_us: None,
        body: "{}".into(),
    };
    store.put_record(rec("factor", "f1", "alice")).await.unwrap();
    store.put_record(rec("factor", "f2", "alice")).await.unwrap();
    store.put_record(rec("challenge", "c", "alice")).await.unwrap();
    assert_eq!(store.records("factor", Some("alice")).await.unwrap().len(), 2);
    assert!(
        store
            .swap_record("factor", "f1", "{}", "{\"x\":1}")
            .await
            .unwrap()
    );
    assert!(
        !store
            .swap_record("factor", "f1", "{}", "{\"x\":2}")
            .await
            .unwrap()
    );
    assert!(store.take_record("challenge", "c").await.unwrap().is_some());
    assert!(store.take_record("challenge", "c").await.unwrap().is_none());
    assert_eq!(store.delete_records_of("alice", &["factor"]).await.unwrap(), 2);

    let session = crate::SessionRecord {
        id_digest: "d1".into(),
        kid: "user:alice".into(),
        created_us: 1,
        last_used_us: 1,
        user: Some("alice".into()),
        facts: "{\"method\":\"local\"}".into(),
    };
    store.put_session(session.clone()).await.unwrap();
    assert_eq!(store.session("d1".into()).await.unwrap(), Some(session));
    assert_eq!(
        store.sessions_of(Some("alice".into()), None).await.unwrap().len(),
        1
    );
    assert_eq!(store.delete_sessions(vec!["d1".into()]).await.unwrap(), 1);

    let token = crate::AdminTokenRecord {
        kid: "k".into(),
        user: "alice".into(),
        verifier: "v".into(),
        description: None,
        created_at: "t".into(),
        expires_at: None,
        access: Some("read_only".into()),
        last_used_us: None,
    };
    store.put_admin_token(token).await.unwrap();
    store.touch_admin_token("k".into(), 9).await.unwrap();
    let back = store.admin_token("k".into()).await.unwrap().unwrap();
    assert_eq!(
        (back.access.as_deref(), back.last_used_us),
        (Some("read_only"), Some(9))
    );
    store.close();
}

#[tokio::test]
async fn the_request_log_and_rollups() {
    let Some(s) = fresh("n").await else { return };
    let store = crate::Store::open_postgres(&s).await.unwrap();
    crate::logs::tests::records(&store).await;
    crate::logs::tests::rollups(&store).await;
    store.close();
}

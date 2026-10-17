use super::*;

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "antirion-store-logs-{name}-{:x}",
        antirion_core::ResourceUid::random().0
    ));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn row(id: &str, started_us: i64, cost: i64, team: &str) -> LogRow {
    LogRow {
        id: id.into(),
        started_us,
        node_id: "n".into(),
        request_id: format!("req-{id}"),
        team: Some(team.into()),
        key: Some("k1".into()),
        model: Some("m".into()),
        status_code: if cost > 50 { 502 } else { 200 },
        response_path: "upstream".into(),
        operation: "chat".into(),
        tags: set(["env:prod", "a"]),
        guardrails: set([]),
        verdicts: set([]),
        categories: set([]),
        latency_ms: cost as f64 * 1.5,
        ttft_ms: -1.0,
        cost_nusd: cost,
        record: format!("{{\"id\":\"{id}\"}}"),
        prompt: Some(format!("hello {id}")),
        content: Some("{}".into()),
        ..LogRow::default()
    }
}

fn order(sort: SortKey, desc: bool, limit: usize) -> LogOrder {
    LogOrder {
        sort,
        desc,
        after: None,
        limit,
    }
}

#[tokio::test]
async fn records_filter_sort_page_and_expire() {
    let dir = temp_dir("list");
    let store = Store::open_embedded(&dir).unwrap();
    records(&store).await;
    store.close();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The record operations, on any backend.
pub(crate) async fn records(store: &Store) {
    let rows: Vec<LogRow> = (0..10)
        .map(|i| {
            row(
                &format!("r{i}"),
                1_000 + i,
                i * 10,
                if i % 2 == 0 { "a" } else { "b" },
            )
        })
        .collect();
    store.insert_logs(rows.clone()).await.unwrap();
    store.insert_logs(vec![rows[0].clone()]).await.unwrap();

    let all = store
        .list_logs(LogFilter::default(), order(SortKey::StartedAt, true, 100))
        .await
        .unwrap();
    assert_eq!(all.len(), 10);
    assert_eq!(all[0].log.id, "r9");

    let team_a = LogFilter {
        any_of: vec![(LogColumn::Team, vec![Arg::Text("a".into())])],
        ..LogFilter::default()
    };
    assert_eq!(store.count_logs(team_a.clone(), 1000).await.unwrap(), 5);
    assert_eq!(
        store.count_logs(team_a, 2).await.unwrap(),
        3,
        "counting stops past the cap"
    );

    let mut seen = Vec::new();
    let mut o = order(SortKey::CostUsd, true, 3);
    loop {
        let page = store.list_logs(LogFilter::default(), o.clone()).await.unwrap();
        if page.is_empty() {
            break;
        }
        o.after = page.last().map(|l| l.pos.clone());
        seen.extend(page.into_iter().map(|l| l.log.id));
    }
    let want: Vec<String> = (0..10).rev().map(|i| format!("r{i}")).collect();
    assert_eq!(seen, want, "pages by cost without duplicates");

    let f = LogFilter {
        status_classes: vec![5],
        latency_min: Some(80.0),
        cost_max_nusd: Some(80),
        tags: vec!["env:prod".into()],
        ..LogFilter::default()
    };
    let hits = store
        .list_logs(f, order(SortKey::LatencyMs, false, 10))
        .await
        .unwrap();
    assert_eq!(
        hits.iter().map(|h| h.log.id.as_str()).collect::<Vec<_>>(),
        ["r6", "r7", "r8"]
    );

    let q = LogFilter {
        q: Some("HELLO R3".into()),
        ..LogFilter::default()
    };
    assert!(
        store
            .list_logs(q.clone(), order(SortKey::StartedAt, true, 5))
            .await
            .unwrap()
            .is_empty()
    );
    let q = LogFilter { q_content: true, ..q };
    assert_eq!(
        store
            .list_logs(q, order(SortKey::StartedAt, true, 5))
            .await
            .unwrap()
            .len(),
        1
    );

    let scoped = LogFilter {
        scopes: Some(vec![ScopeRef::Team("b".into())]),
        ..LogFilter::default()
    };
    assert_eq!(store.count_logs(scoped, 100).await.unwrap(), 5);
    let none = LogFilter {
        scopes: Some(Vec::new()),
        ..LogFilter::default()
    };
    assert_eq!(store.count_logs(none, 100).await.unwrap(), 0);

    assert!(
        store
            .update_log("r1", "{\"id\":\"r1\",\"x\":1}".into())
            .await
            .unwrap()
    );
    assert_eq!(
        store.log("r1").await.unwrap().unwrap().record,
        "{\"id\":\"r1\",\"x\":1}"
    );
    assert!(!store.update_log("nope", "{}".into()).await.unwrap());

    assert_eq!(store.expire_logs(1_005, 1_008).await.unwrap(), 5);
    let left = store
        .list_logs(LogFilter::default(), order(SortKey::StartedAt, false, 100))
        .await
        .unwrap();
    assert_eq!(left.len(), 5);
    assert!(
        left.iter()
            .all(|l| (l.log.started_us >= 1_008) == l.log.content.is_some())
    );
    store.clear_logs().await.unwrap();
    assert!(store.log("r9").await.unwrap().is_none());
}

fn concat(a: &[u8], b: &[u8]) -> Vec<u8> {
    [a, b].concat()
}

#[tokio::test]
async fn rollups_add_up_per_period_and_dimensions() {
    let dir = temp_dir("rollups");
    let store = Store::open_embedded(&dir).unwrap();
    rollups(&store).await;
    store.close();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The rollup operations, on any backend.
pub(crate) async fn rollups(store: &Store) {
    let mut dims = vec![None; ROLLUP_DIMS.len()];
    dims[5] = Some("m".to_owned());
    dims[11] = Some("503".to_owned());
    let r = RollupRow {
        grain_s: 60,
        start_us: 60_000_000,
        node_id: "n".into(),
        dims: dims.clone(),
        counts: vec![1; MEASURES.len()],
        sketches: vec![vec![1]; SKETCHES.len()],
    };
    store.merge_rollups(vec![r.clone()], concat).await.unwrap();
    store.merge_rollups(vec![r.clone()], concat).await.unwrap();
    let mut other = r.clone();
    other.dims[5] = Some("n".into());
    other.dims[11] = Some("200".into());
    store.merge_rollups(vec![other], concat).await.unwrap();

    let q = RollupQuery {
        grain_s: 60,
        from_us: 0,
        to_us: 120_000_000,
        any_of: vec![(5, vec!["m".into()])],
        ..RollupQuery::default()
    };
    let rows = store.rollups(q).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].counts, vec![2; MEASURES.len()]);
    assert_eq!(rows[0].sketches[0], vec![1, 1]);
    assert_eq!(rows[0].dims, dims);

    let errors = RollupQuery {
        grain_s: 60,
        from_us: 0,
        to_us: 120_000_000,
        status_classes: vec![5],
        ..RollupQuery::default()
    };
    assert_eq!(store.rollups(errors).await.unwrap().len(), 1);
    let later = RollupQuery {
        grain_s: 60,
        from_us: 60_000_001,
        to_us: 120_000_000,
        ..RollupQuery::default()
    };
    assert!(store.rollups(later).await.unwrap().is_empty());
}

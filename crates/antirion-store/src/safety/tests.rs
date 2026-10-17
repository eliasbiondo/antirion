use super::*;

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "antirion-store-safety-{name}-{:x}",
        antirion_core::ResourceUid::random().0
    ));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn event(id: &str, time_us: i64, category: &str, team: &str) -> SafetyEventRow {
    SafetyEventRow {
        id: id.into(),
        time_us,
        log_id: format!("log-{id}"),
        request_id: format!("req-{id}"),
        node_id: "n".into(),
        guardrail: "g".into(),
        guardrail_type: "regex".into(),
        stage: "input".into(),
        action: "block".into(),
        category: category.into(),
        severity: "high".into(),
        confidence: 1.0,
        entities: "[]".into(),
        fingerprint: format!("fp-{category}"),
        team: Some(team.into()),
        review_state: "open".into(),
        ..SafetyEventRow::default()
    }
}

fn concat(a: &[u8], b: &[u8]) -> Vec<u8> {
    [a, b].concat()
}

#[tokio::test]
async fn events_list_newest_first_with_filters_scopes_and_labels() {
    let dir = temp_dir("events");
    let store = Store::Sqlite(crate::SqliteStore::open(&dir).unwrap());
    let rows = vec![
        event("a", 10, "pii", "t1"),
        event("b", 20, "secret", "t1"),
        event("c", 30, "pii", "t2"),
    ];
    store.insert_safety_events(rows.clone()).await.unwrap();
    store.insert_safety_events(rows).await.unwrap();
    let all = store
        .list_safety_events(&SafetyFilter::default(), None, 10)
        .await
        .unwrap();
    assert_eq!(
        all.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["c", "b", "a"]
    );
    let page = store
        .list_safety_events(&SafetyFilter::default(), Some((30, "c".into())), 1)
        .await
        .unwrap();
    assert_eq!(page[0].id, "b");
    let pii = SafetyFilter {
        any_of: vec![(SafetyColumn::Category, vec!["pii".into()])],
        scopes: Some(vec![ScopeRef::Team("t1".into())]),
        ..SafetyFilter::default()
    };
    let got = store.list_safety_events(&pii, None, 10).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "a");
    let q = SafetyFilter {
        q: Some("REQ-B".into()),
        ..SafetyFilter::default()
    };
    assert_eq!(store.count_safety_events(&q, 100).await.unwrap(), 1);
    assert!(
        store
            .review_safety_event("a", "false_positive", "alice", 40, Some("ok"))
            .await
            .unwrap()
    );
    assert!(
        !store
            .review_safety_event("zz", "reviewed", "alice", 40, None)
            .await
            .unwrap()
    );
    let a = store.safety_event("a").await.unwrap().unwrap();
    assert_eq!(
        (a.review_state.as_str(), a.reviewed_by.as_deref()),
        ("false_positive", Some("alice"))
    );
    assert_eq!(store.review_counts(0).await.unwrap(), [("g".to_owned(), 1, 1)]);
    assert_eq!(store.expire_safety(25, None).await.unwrap(), 2);
    assert_eq!(
        store
            .count_safety_events(&SafetyFilter::default(), 100)
            .await
            .unwrap(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn hours_merge_per_node() {
    let dir = temp_dir("hours");
    let store = Store::Sqlite(crate::SqliteStore::open(&dir).unwrap());
    let h = GuardrailHour {
        guardrail: "g".into(),
        hour_us: 3_600_000_000,
        node_id: "n".into(),
        evaluations: 2,
        hits: 1,
        latency: vec![1],
        ..GuardrailHour::default()
    };
    store
        .merge_guardrail_hours(vec![h.clone()], concat)
        .await
        .unwrap();
    store
        .merge_guardrail_hours(vec![h.clone()], concat)
        .await
        .unwrap();
    let other = GuardrailHour {
        node_id: "m".into(),
        ..h
    };
    store.merge_guardrail_hours(vec![other], concat).await.unwrap();
    let mut got = store.guardrail_hours(0).await.unwrap();
    got.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    assert_eq!(got.len(), 2);
    assert_eq!(
        (got[1].evaluations, got[1].hits, got[1].latency.clone()),
        (4, 2, vec![1, 1])
    );
    let _ = std::fs::remove_dir_all(&dir);
}

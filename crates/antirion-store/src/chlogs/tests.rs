use super::*;

#[test]
fn urls_are_taken_apart() {
    assert_eq!(
        split_url("http://u:p@ch.internal:8123").unwrap(),
        (
            "http://ch.internal:8123".into(),
            Some("u".into()),
            Some("p".into())
        )
    );
    assert_eq!(
        split_url("https://ch:8443/").unwrap(),
        ("https://ch:8443/".into(), None, None)
    );
    assert!(split_url("tcp://ch:9000").is_err());
    assert!(split_url("http://u@").is_err());
}

fn row(id: &str, started_us: i64, user: &str) -> LogRow {
    LogRow {
        id: id.into(),
        started_us,
        node_id: "n".into(),
        request_id: format!("r-{id}"),
        project: Some("p".into()),
        end_user: Some(user.into()),
        status_code: 200,
        response_path: "upstream".into(),
        operation: "chat".into(),
        tags: crate::logs::set(["prod"]),
        guardrails: crate::logs::set([]),
        verdicts: crate::logs::set([]),
        categories: crate::logs::set([]),
        latency_ms: 12.5,
        ttft_ms: -1.0,
        cost_nusd: started_us,
        record: format!("{{\"id\":\"{id}\"}}"),
        content: Some("{\"messages\":[]}".into()),
        prompt: Some("hello".into()),
        ..LogRow::default()
    }
}

/// Runs against a server when `ANTIRION_TEST_CLICKHOUSE_URL` is set, for example
/// `docker run -d -p 18123:8123 clickhouse/clickhouse-server` and `http://default@127.0.0.1:18123`.
#[tokio::test]
async fn records_round_trip_on_a_server() {
    let Ok(url) = std::env::var("ANTIRION_TEST_CLICKHOUSE_URL") else {
        return;
    };
    let database = format!("antirion_test_{:x}", antirion_core::ResourceUid::random().0);
    let s = ClickHouseLogs::new(ClickHouseSettings {
        url: antirion_core::Secret::new(url),
        database,
        batch_size: 2,
        flush_interval: Duration::from_secs(1),
        insert_timeout: Duration::from_secs(10),
    })
    .unwrap();
    let rows: Vec<LogRow> = (0..5)
        .map(|i| {
            row(
                &format!("id{i}"),
                1_000 + i,
                if i % 2 == 0 { "even" } else { "odd" },
            )
        })
        .collect();
    s.insert(rows).await.unwrap();
    let all = LogFilter::default();
    assert_eq!(s.count(&all, 100).await.unwrap(), 5);
    let order = LogOrder {
        sort: SortKey::CostUsd,
        desc: true,
        after: None,
        limit: 2,
    };
    let page = s.list(&all, &order).await.unwrap();
    assert_eq!(
        page.iter().map(|l| l.log.id.as_str()).collect::<Vec<_>>(),
        ["id4", "id3"]
    );
    let next = LogOrder {
        after: Some(page[1].pos.clone()),
        ..order
    };
    let page2 = s.list(&all, &next).await.unwrap();
    assert_eq!(
        page2.iter().map(|l| l.log.id.as_str()).collect::<Vec<_>>(),
        ["id2", "id1"]
    );
    let tagged = LogFilter {
        tags: vec!["prod".into()],
        q: Some("ID1".into()),
        ..LogFilter::default()
    };
    assert_eq!(s.count(&tagged, 100).await.unwrap(), 1);
    assert!(s.update("id1", "{\"id\":\"id1\",\"x\":1}".into()).await.unwrap());
    assert!(!s.update("missing", "{}".into()).await.unwrap());
    let one = s.log("id1").await.unwrap().unwrap();
    assert_eq!(one.record, "{\"id\":\"id1\",\"x\":1}");
    assert_eq!(
        s.count(&all, 100).await.unwrap(),
        5,
        "an update is a newer version, not a new record"
    );
    let mut odd = LogFilter::default();
    odd.any_of
        .push((LogColumn::EndUser, vec![Arg::Text("odd".into())]));
    assert_eq!(s.ids(&odd).await.unwrap().len(), 2);
    assert_eq!(s.delete(&odd).await.unwrap(), 2);
    assert_eq!(s.count(&all, 100).await.unwrap(), 3);
    assert_eq!(s.expire(1_001, 2_000).await.unwrap(), 1);
    assert_eq!(s.count(&all, 100).await.unwrap(), 2);
    s.clear().await.unwrap();
    assert_eq!(s.count(&all, 100).await.unwrap(), 0);
    let _ = s
        .server
        .query("DROP DATABASE IF EXISTS ?")
        .bind(Identifier(&s.database))
        .execute()
        .await;
}

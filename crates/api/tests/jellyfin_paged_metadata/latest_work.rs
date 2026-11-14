//! Latest has no wire total: it selects bounded references before hydration.
use super::*;
use media_server::provider::QueryTiming;
use media_server::query_metrics::{QueryMetrics, QueryMetricsSnapshot};

async fn latest_observed(app: &axum::Router, uri: &str) -> (Value, QueryMetricsSnapshot) {
    let metrics = QueryMetrics::new(QueryTiming::start(), true);
    let mut request = Request::builder()
        .uri(uri)
        .header("Authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(metrics.clone());
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (serde_json::from_slice(&body).unwrap(), metrics.snapshot())
}

#[tokio::test]
async fn latest_skips_exact_count_and_bounds_hydration() {
    let mut work = Vec::new();
    for count in [20, 200] {
        let fixture = tv_fixture(count, 1);
        let app = router(fixture.state.clone());
        let (body, stats) = latest_observed(
            &app,
            &format!(
                "/Users/me/Items/Latest?ParentId={}&IsPlayed=false&Limit=5",
                fixture.library_id
            ),
        )
        .await;
        assert_eq!(body.as_array().unwrap().len(), 5);
        assert_eq!(
            fixture.state.jellyfin_candidates().load(Ordering::SeqCst),
            5
        );
        assert_eq!(fixture.files.subtitles(), 5);
        assert_eq!(
            stats.total, None,
            "Latest must not compute an unused exact total"
        );
        work.push(stats.vm_steps.expect("diagnostic SQL work"));
    }
    assert!(
        work[1] <= work[0] + 2500,
        "Latest work must not grow with unselected matching works: {work:?}"
    );
}

#[tokio::test]
async fn latest_offsets_cross_played_boundary_without_duplicates() {
    let fixture = tv_fixture(8, 1);
    let store = fixture.state.store();
    let user = store.admin_user_id().unwrap();
    for index in [1, 3, 5] {
        let media = media_for_title(&fixture.state, &format!("Show{index:03}"));
        store
            .upsert_unit(
                user,
                media,
                -1,
                -1,
                0,
                Some(true),
                None,
                None,
                None,
                None,
                false,
                10,
            )
            .unwrap();
    }
    let app = router(fixture.state.clone());
    let base = format!("/Users/me/Items/Latest?ParentId={}", fixture.library_id);
    let all = get_json(&app, &format!("{base}&Limit=20")).await;
    let all = all.as_array().unwrap();
    assert_eq!(all.len(), 8);
    assert!(
        all[..5]
            .iter()
            .all(|item| item["UserData"]["Played"] == false)
    );
    assert!(
        all[5..]
            .iter()
            .all(|item| item["UserData"]["Played"] == true)
    );
    for offset in [0, 3, 4, 5, 6, 8, 9] {
        let page = get_json(&app, &format!("{base}&StartIndex={offset}&Limit=3")).await;
        let expected: Vec<_> = all.iter().skip(offset).take(3).cloned().collect();
        assert_eq!(page.as_array().unwrap(), &expected, "offset={offset}");
    }
    for (played, count) in [(false, 5), (true, 3)] {
        let filtered = get_json(&app, &format!("{base}&IsPlayed={played}&Limit=20")).await;
        let rows = filtered.as_array().unwrap();
        assert_eq!(rows.len(), count);
        assert!(rows.iter().all(|item| item["UserData"]["Played"] == played));
    }
}

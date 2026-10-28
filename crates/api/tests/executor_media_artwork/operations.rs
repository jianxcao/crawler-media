use super::*;

fn insert_merge_pair(state: &ApiState) -> Value {
    let survivor = MediaId::new();
    let loser = MediaId::new();
    let handle = state.store();
    let store = handle.lock();
    for id in [survivor, loser] {
        store
            .insert_media(&Media {
                id,
                kind: MediaKind::Movie,
                title: "Merge fixture".into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
    }
    json!({
        "request_id": MediaId::new().to_string(),
        "survivor": survivor.to_string(), "loser": loser.to_string(),
        "survivor_revision": 0, "loser_revision": 0,
    })
}

async fn assert_not_started_then_retry(app: &axum::Router, token: &str, body: Value) {
    let uri = format!(
        "/api/v1/media/operations/{}",
        body["request_id"].as_str().unwrap()
    );
    let response = app
        .clone()
        .oneshot(request("GET", &uri, token, Value::Null))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "a busy write must leave no operation"
    );
    let response = app
        .clone()
        .oneshot(request("POST", "/api/v1/media/merges", token, body))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "retry must be able to freeze the original pair"
    );
    let response = app
        .clone()
        .oneshot(request("GET", &uri, token, Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let status: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status["data"]["state"], "frozen");
}

#[test]
fn merge_write_rejects_full_capacity_without_queuing_a_late_write() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token) = state_with_admin(tmp.path());
    let body = insert_merge_pair(&state);
    let reservations: Vec<_> = (0..8)
        .map(|_| state.http_work.try_reserve().unwrap())
        .collect();
    let app = router(state);
    let runtime = runtime();
    runtime.block_on(async {
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/media/merges",
                &token,
                body.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["retry-after"], "1");
        drop(reservations);
        assert_not_started_then_retry(&app, &token, body).await;
    });
}

#[test]
fn merge_write_bounds_store_wait_without_queuing_a_late_write() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, token) = state_with_admin(tmp.path());
    let body = insert_merge_pair(&state);
    let app = router(state.clone());
    let runtime = runtime();
    let store = state.store();
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = store.lock();
        locked_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
    });
    locked_rx.recv().unwrap();
    let response = runtime
        .block_on(app.clone().oneshot(request(
            "POST",
            "/api/v1/media/merges",
            &token,
            body.clone(),
        )))
        .unwrap();
    holder.join().unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["retry-after"], "1");
    runtime.block_on(assert_not_started_then_retry(&app, &token, body));
}

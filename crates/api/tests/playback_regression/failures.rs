use super::*;

fn database(root: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(root.join("data/subscribe.db")).unwrap()
}

async fn web_event(
    app: &axum::Router,
    media: MediaId,
    event: &str,
    position: i64,
) -> axum::response::Response {
    send(app, "POST", "/api/v1/playback/progress", "Bearer admin-token",
        json!({"media_item_id":media.to_string(),"device_id":"browser","event":event,"position_ms":position}),None).await
}

#[tokio::test]
async fn session_write_failure_rolls_back_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, _) = app(tmp.path());
    let db = database(tmp.path());
    db.execute_batch("CREATE TRIGGER block_session BEFORE INSERT ON playback_sessions BEGIN SELECT RAISE(ABORT,'session blocked'); END;").unwrap();
    assert_eq!(
        web_event(&app, media, "start", 30000).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let store = Store::open(tmp.path().join("data")).unwrap();
    assert!(store.unit_state(admin(), media, -1, -1).unwrap().is_none());
    assert!(store.get_session(admin(), "browser").unwrap().is_none());
}

#[tokio::test]
async fn stop_log_failure_retains_session_and_prior_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, _) = app(tmp.path());
    assert_eq!(
        web_event(&app, media, "start", 1000).await.status(),
        StatusCode::OK
    );
    let db = database(tmp.path());
    db.execute_batch("CREATE TRIGGER block_log BEFORE INSERT ON playback_logs BEGIN SELECT RAISE(ABORT,'log blocked'); END;").unwrap();
    assert_eq!(
        web_event(&app, media, "stop", 30000).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let store = Store::open(tmp.path().join("data")).unwrap();
    assert_eq!(
        store
            .unit_state(admin(), media, -1, -1)
            .unwrap()
            .unwrap()
            .position_ms,
        1000
    );
    assert_eq!(
        store
            .get_session(admin(), "browser")
            .unwrap()
            .unwrap()
            .position_ms,
        1000
    );
    assert!(
        store
            .list_logs(10, None, None, None, i64::MAX / 2, None)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn legacy_migration_failure_returns_error_without_shadowing() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .upsert_unit(
            admin(),
            media,
            0,
            0,
            45000,
            Some(true),
            Some(true),
            Some(120000),
            Some("embedded:1"),
            Some("off"),
            true,
            1,
        )
        .unwrap();
    let db = database(tmp.path());
    db.execute_batch("CREATE TRIGGER block_copy BEFORE INSERT ON playback_units WHEN NEW.season=-1 BEGIN SELECT RAISE(ABORT,'copy blocked'); END;").unwrap();
    assert_eq!(
        web_event(&app, media, "start", 45000).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        send(
            &app,
            "POST",
            "/Sessions/Playing",
            "Bearer admin-token",
            json!({"ItemId":id,"PositionTicks":450000000}),
            Some("actual-device")
        )
        .await
        .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(store.unit_state(admin(), media, -1, -1).unwrap().is_none());
    assert!(
        store
            .unit_state(admin(), media, 0, 0)
            .unwrap()
            .unwrap()
            .favorite
    );
}

#[tokio::test]
async fn old_logs_without_real_identity_are_not_revocable() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, _) = app(tmp.path());
    let db = database(tmp.path());
    db.execute("INSERT INTO playback_logs (id,user_id,media_id,client,device_name,play_method,started_at,ended_at) VALUES ('old',?1,?2,'Infuse','Bedroom Apple TV','DirectPlay',1,2)",rusqlite::params![admin().to_string(),media.to_string()]).unwrap();
    let body = json(
        send(
            &app,
            "GET",
            "/api/v1/playback/devices",
            "Bearer admin-token",
            json!({}),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(body["data"][0]["revocable"], false);
    let legacy_id = body["data"][0]["device_id"].as_str().unwrap();
    let response = send(
        &app,
        "DELETE",
        &format!("/api/v1/playback/devices/{legacy_id}?user_id={}", admin()),
        "Bearer admin-token",
        json!({}),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(response).await["error"]["code"],
        "playback.unsupported"
    );
}

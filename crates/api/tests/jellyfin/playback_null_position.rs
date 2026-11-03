use super::*;
use std::sync::atomic::Ordering;

async fn event(app: &axum::Router, path: &str, body: Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .header("X-Emby-Device-Id", "null-position-device")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

fn user() -> domain::UserId {
    domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap()
}

#[tokio::test]
async fn playing_without_position_starts_a_session_for_later_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    event(&app, "/Sessions/Playing", json!({"ItemId": id})).await;
    assert!(
        store
            .get_session(user(), "null-position-device")
            .unwrap()
            .is_some()
    );
    assert_eq!(store.playback_progress(user(), media).unwrap(), None);

    event(
        &app,
        "/Sessions/Playing/Progress",
        json!({"ItemId": id, "PositionTicks": 50000000}),
    )
    .await;
    assert_eq!(store.playback_progress(user(), media).unwrap(), Some(5000));
}

#[tokio::test]
async fn null_position_pause_and_stop_preserve_progress_and_close_session() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    event(
        &app,
        "/Sessions/Playing",
        json!({"ItemId": id, "PositionTicks": 0}),
    )
    .await;
    event(
        &app,
        "/Sessions/Playing/Progress",
        json!({"ItemId": id, "PositionTicks": 50000000}),
    )
    .await;
    event(
        &app,
        "/Sessions/Playing/Progress",
        json!({"ItemId": id, "PositionTicks": null, "IsPaused": true}),
    )
    .await;
    let session = store
        .get_session(user(), "null-position-device")
        .unwrap()
        .unwrap();
    assert!(session.paused);
    assert_eq!(session.position_ms, 5000);
    assert_eq!(store.playback_progress(user(), media).unwrap(), Some(5000));

    event(
        &app,
        "/Sessions/Playing/Stopped",
        json!({"ItemId": id, "PositionTicks": null}),
    )
    .await;
    assert!(
        store
            .get_session(user(), "null-position-device")
            .unwrap()
            .is_none()
    );
    assert_eq!(store.playback_progress(user(), media).unwrap(), Some(5000));
    let item = app
        .oneshot(
            Request::builder()
                .uri(format!("/Items/{id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let data = json(item).await;
    assert_eq!(data["UserData"]["PlaybackPositionTicks"], 50000000);
    assert_eq!(data["UserData"]["Played"], false);
    assert_eq!(data["UserData"]["PlayCount"], 0);
}

#[tokio::test]
async fn null_position_breaks_watched_time_sampling() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock(std::sync::atomic::AtomicI64::new(1_000_000)));
    let (app, _, id) = app_with_clock(tmp.path(), clock.clone());
    let store = Store::open(tmp.path().join("data")).unwrap();
    event(
        &app,
        "/Sessions/Playing",
        json!({"ItemId": id, "PositionTicks": 0}),
    )
    .await;
    clock.0.store(1_002_000, Ordering::SeqCst);
    event(
        &app,
        "/Sessions/Playing/Progress",
        json!({"ItemId": id, "PositionTicks": 20000000}),
    )
    .await;
    clock.0.store(1_004_000, Ordering::SeqCst);
    event(
        &app,
        "/Sessions/Playing/Progress",
        json!({"ItemId": id, "PositionTicks": null}),
    )
    .await;
    clock.0.store(1_006_000, Ordering::SeqCst);
    event(
        &app,
        "/Sessions/Playing/Progress",
        json!({"ItemId": id, "PositionTicks": 60000000}),
    )
    .await;
    let session = store
        .get_session(user(), "null-position-device")
        .unwrap()
        .unwrap();
    assert_eq!(session.position_ms, 6000);
    assert_eq!(session.watched_ms, 2000);
    clock.0.store(1_008_000, Ordering::SeqCst);
    event(
        &app,
        "/Sessions/Playing/Stopped",
        json!({"ItemId": id, "PositionTicks": null}),
    )
    .await;
    let logs = store
        .list_logs(10, None, None, None, 1008, Some(user()))
        .unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].end_position_ms, 6000);
    assert_eq!(logs[0].watched_ms, 2000);
}

#[tokio::test]
async fn null_position_cannot_reopen_a_closed_binding() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let sid = play_session_for(&app, &id, Some("null-position-device")).await;
    event(
        &app,
        "/Sessions/Playing",
        json!({"ItemId": id, "PositionTicks": 0, "PlaySessionId": sid}),
    )
    .await;
    event(
        &app,
        "/Sessions/Playing/Stopped",
        json!({"ItemId": id, "PositionTicks": 50000000, "PlaySessionId": sid}),
    )
    .await;
    event(
        &app,
        "/Sessions/Playing/Stopped",
        json!({"ItemId": id, "PositionTicks": null, "PlaySessionId": sid}),
    )
    .await;
    event(
        &app,
        "/Sessions/Playing",
        json!({"ItemId": id, "PositionTicks": null, "PlaySessionId": sid}),
    )
    .await;
    event(
        &app,
        "/Sessions/Playing/Progress",
        json!({"ItemId": id, "PositionTicks": 70000000, "PlaySessionId": sid}),
    )
    .await;
    assert!(
        store
            .get_session(user(), "null-position-device")
            .unwrap()
            .is_none()
    );
    assert_eq!(store.playback_progress(user(), media).unwrap(), Some(5000));
}

#[tokio::test]
async fn null_position_start_preserves_existing_source_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let item = domain::TypedItemId::Work(media);
    event(
        &app,
        "/Sessions/Playing",
        json!({"ItemId": id, "PositionTicks": 0}),
    )
    .await;
    event(
        &app,
        "/Sessions/Playing/Stopped",
        json!({"ItemId": id, "PositionTicks": 50000000}),
    )
    .await;
    let before = store.resolve_playback(user(), &item, None, None).unwrap();
    assert_eq!(before.start_ms, 5000);
    event(
        &app,
        "/Sessions/Playing",
        json!({"ItemId": id, "PositionTicks": null}),
    )
    .await;
    let after = store.resolve_playback(user(), &item, None, None).unwrap();
    assert_eq!(after.start_ms, 5000);
    assert_eq!(after.target.file.file_id, before.target.file.file_id);
    assert_eq!(after.target.file.version, before.target.file.version);
    assert!(
        store
            .get_session(user(), "null-position-device")
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn null_position_events_from_replaced_source_do_not_change_session() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media, id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let sid = play_session_for(&app, &id, Some("null-position-device")).await;
    event(
        &app,
        "/Sessions/Playing",
        json!({"ItemId": id, "PositionTicks": 50000000, "PlaySessionId": sid}),
    )
    .await;
    let before = store
        .get_session(user(), "null-position-device")
        .unwrap()
        .unwrap();
    let old = store
        .resolve_playback(user(), &domain::TypedItemId::Work(media), None, None)
        .unwrap()
        .target
        .file;
    let mut next = old.clone();
    next.version.revision += 1;
    next.version.source_version = "replacement-source".into();
    store
        .publish_files(
            &store::library_write::WriteScope {
                library_id: old.library_id,
                config_revision: store
                    .library_config_revision(&old.library_id.to_string())
                    .unwrap(),
            },
            &[store::library_write::FilePublication {
                file: &next,
                units: &[domain::UnitKey::WHOLE],
                expected_version: Some(&old.version),
                nfo_observation: None,
                probe_required: false,
            }],
            2,
        )
        .unwrap();
    for path in [
        "/Sessions/Playing",
        "/Sessions/Playing/Progress",
        "/Sessions/Playing/Stopped",
    ] {
        event(
            &app,
            path,
            json!({"ItemId": id, "PositionTicks": null, "PlaySessionId": sid}),
        )
        .await;
    }
    assert_eq!(
        store.get_session(user(), "null-position-device").unwrap(),
        Some(before)
    );
    assert_eq!(store.playback_progress(user(), media).unwrap(), Some(5000));
}

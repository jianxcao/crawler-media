use super::*;
use media_server::provider::MediaServerProvider;

fn standard_auth(device: &str) -> String {
    format!(
        "MediaBrowser Token=\"admin-token\", DeviceId=\"{device}\", Client=\"Infuse\", Device=\"Same Name\", Version=\"7.1\""
    )
}

#[tokio::test]
async fn standard_identity_metadata_and_stop_are_independent() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, id) = app(tmp.path());
    for device in ["bedroom", "living-room"] {
        assert_eq!(
            send(
                &app,
                "POST",
                "/Sessions/Playing",
                &standard_auth(device),
                json!({"ItemId":id,"PositionTicks":0}),
                None
            )
            .await
            .status(),
            StatusCode::NO_CONTENT
        );
    }
    let store = Store::open(tmp.path().join("data")).unwrap();
    let bedroom = store.get_session(admin(), "bedroom").unwrap().unwrap();
    assert_eq!(bedroom.device_name.as_deref(), Some("Same Name"));
    assert_eq!(bedroom.client_version.as_deref(), Some("7.1"));
    assert!(bedroom.revocable());
    assert_eq!(
        send(
            &app,
            "POST",
            "/Sessions/Playing/Stopped",
            &standard_auth("bedroom"),
            json!({"ItemId":id,"PositionTicks":10000000}),
            None
        )
        .await
        .status(),
        StatusCode::NO_CONTENT
    );
    assert!(store.get_session(admin(), "bedroom").unwrap().is_none());
    assert!(store.get_session(admin(), "living-room").unwrap().is_some());
    let devices = json(
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
    assert_eq!(
        devices["data"].as_array().unwrap().len(),
        2,
        "same display name must not merge devices"
    );
    let revoke = format!("/api/v1/playback/devices/bedroom?user_id={}", admin());
    assert_eq!(
        send(
            &app,
            "DELETE",
            &revoke,
            "Bearer admin-token",
            json!({}),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    let stream = format!("/Videos/{id}/stream");
    assert_eq!(
        send(
            &app,
            "GET",
            &stream,
            &standard_auth("bedroom"),
            json!({}),
            None
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(
            &app,
            "GET",
            &stream,
            &standard_auth("living-room"),
            json!({}),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        send(
            &app,
            "POST",
            "/Sessions/Playing",
            &standard_auth("bedroom"),
            json!({"ItemId":id,"PositionTicks":0}),
            None
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn missing_protocol_identity_is_correlated_but_not_revocable() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, id) = app(tmp.path());
    for name in ["Bedroom", "Living Room"] {
        let auth =
            format!("MediaBrowser Token=\"admin-token\", Client=\"Infuse\", Device=\"{name}\"");
        assert_eq!(
            send(
                &app,
                "POST",
                "/Sessions/Playing",
                &auth,
                json!({"ItemId":id,"PositionTicks":0}),
                None
            )
            .await
            .status(),
            StatusCode::NO_CONTENT
        );
    }
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
    let devices = body["data"].as_array().unwrap();
    assert_eq!(devices.len(), 2);
    for device in devices {
        assert_eq!(device["revocable"], false);
        assert!(
            device["device_id"]
                .as_str()
                .unwrap()
                .starts_with("unidentified:")
        );
        assert!(
            !device["device_id"]
                .as_str()
                .unwrap()
                .contains("admin-token")
        );
    }
}

fn provider(root: &Path) -> api::media_server_provider::ApiServerProvider {
    api::media_server_provider::ApiServerProvider::new(
        ApiState::new(
            Store::open(root.join("data")).unwrap(),
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(root.join("stage"))),
            root.join("library"),
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn direct_jellyfin_progress_preserves_legacy_preferences_and_completion() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, media, id) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .upsert_unit(
            admin(),
            media,
            0,
            0,
            45000,
            Some(false),
            Some(true),
            Some(120000),
            Some("embedded:1"),
            Some("off"),
            true,
            1,
        )
        .unwrap();
    provider(tmp.path())
        .update_progress(admin(), &id, 120000, false)
        .await
        .unwrap();
    let unit = store.unit_state(admin(), media, -1, -1).unwrap().unwrap();
    assert!(unit.played);
    assert!(unit.favorite);
    assert_eq!(unit.duration_ms, Some(120000));
    assert_eq!(unit.audio_track.as_deref(), Some("embedded:1"));
    assert_eq!(unit.subtitle_track.as_deref(), Some("off"));
    assert_eq!(unit.play_count, 1);
}

#[tokio::test]
async fn direct_jellyfin_progress_migration_failure_does_not_shadow_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, media, id) = app(tmp.path());
    let provider = provider(tmp.path());
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
            None,
            None,
            true,
            1,
        )
        .unwrap();
    let db = rusqlite::Connection::open(tmp.path().join("data/subscribe.db")).unwrap();
    db.execute_batch("CREATE TRIGGER block_direct_copy BEFORE INSERT ON playback_units WHEN NEW.season=-1 BEGIN SELECT RAISE(ABORT,'migration blocked'); END;").unwrap();
    assert!(
        provider
            .update_progress(admin(), &id, 45000, false)
            .await
            .is_err()
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
async fn artwork_remains_public_and_streams_require_authentication() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, id) = app(tmp.path());
    std::fs::write(tmp.path().join("poster.jpg"), b"poster").unwrap();
    let poster = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/posters/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(poster.status(), StatusCode::OK);
    let stream = app
        .oneshot(
            Request::get(format!("/Videos/{id}/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stream.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn metrics_are_best_effort_but_do_not_accept_invalid_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, id) = app(tmp.path());
    let invalid = send(
        &app,
        "POST",
        "/api/v1/playback/metrics",
        "Bearer admin-token",
        json!({"library_file_id":"invalid","watched_ms":1000}),
        None,
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::OK);
    let db = rusqlite::Connection::open(tmp.path().join("data/subscribe.db")).unwrap();
    db.execute_batch("CREATE TRIGGER block_metric BEFORE INSERT ON playback_metrics BEGIN SELECT RAISE(ABORT,'metric blocked'); END;").unwrap();
    assert_eq!(
        send(
            &app,
            "POST",
            "/api/v1/playback/metrics",
            "Bearer admin-token",
            json!({"library_file_id":id,"watched_ms":1000}),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM playback_metrics", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

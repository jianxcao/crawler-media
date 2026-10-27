//! Storage cutover uses one current configuration contract and signed Playback identity.
#[path = "support/library_presence.rs"]
mod fixture;

use axum::http::StatusCode;
use fixture::PresenceFixture;
use serde_json::{Value, json};

#[tokio::test]
async fn transfer_settings_patch_is_cas_and_null_clears_without_touching_roots() {
    let f = PresenceFixture::new();
    let roots = f.state.store().lock().list_library_roots().unwrap();
    let (status, initial) = f
        .request("GET", "/api/v1/settings/transfer", Some(&f.admin_token))
        .await;
    assert_eq!(status, StatusCode::OK);
    let revision = initial["data"]["revision"].as_i64().unwrap();
    let (status, saved) = f.request_with_body("PATCH", "/api/v1/settings/transfer", Some(&f.admin_token), json!({
        "expected_revision": revision, "transfer_mode": "copy", "watch_intake": "/incoming", "scrape": true,
    })).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let current = saved["data"]["revision"].as_i64().unwrap();
    let (status, _) = f
        .request_with_body(
            "PATCH",
            "/api/v1/settings/transfer",
            Some(&f.admin_token),
            json!({
                "expected_revision": revision, "transfer_mode": "move",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, cleared) = f
        .request_with_body(
            "PATCH",
            "/api/v1/settings/transfer",
            Some(&f.admin_token),
            json!({
                "expected_revision": current, "watch_intake": null,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["data"]["watch_intake"], Value::Null);
    assert_eq!(cleared["data"]["transfer_mode"], "copy");
    assert_eq!(cleared["data"]["scrape"], true);
    assert_eq!(f.state.store().lock().watch_intake().unwrap(), None);
    assert_eq!(f.state.store().lock().list_library_roots().unwrap(), roots);
}

#[tokio::test]
async fn transfer_settings_reject_unknown_and_bad_fields_without_writes() {
    let f = PresenceFixture::new();
    for body in [
        json!({"expected_revision":0,"movie_root":"/outside"}),
        json!({"expected_revision":0,"transfer_mode":"bogus"}),
        json!({"expected_revision":0,"watch_intake":true}),
        json!({"expected_revision":0,"transfer_mode":null}),
        json!({"expected_revision":0,"scrape":null}),
        json!({"transfer_mode":"copy"}),
    ] {
        let (status, response) = f
            .request_with_body(
                "PATCH",
                "/api/v1/settings/transfer",
                Some(&f.admin_token),
                body,
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        assert_eq!(f.state.store().lock().transfer_mode().unwrap(), "hardlink");
    }
}

#[tokio::test]
async fn removed_directory_endpoints_are_not_available() {
    let f = PresenceFixture::new();
    for (method, path) in [
        ("GET", "/api/v1/directory"),
        ("PUT", "/api/v1/directory"),
        ("POST", "/api/v1/directory/roots"),
        ("DELETE", "/api/v1/directory/roots/unused"),
    ] {
        let (status, _) = f
            .request_with_body(method, path, Some(&f.admin_token), json!({}))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
    }
}

#[tokio::test]
async fn playback_requires_issued_session_and_nonnegative_sequence_before_any_write() {
    let f = PresenceFixture::new();
    let (media, file) = f.insert_media_and_ledger("Movie.mkv");
    for extra in [
        json!({}),
        json!({"play_session_id":null,"event_sequence":1}),
        json!({"play_session_id":"forged"}),
        json!({"play_session_id":"forged","event_sequence":1}),
        json!({"play_session_id":"forged","event_sequence":-1}),
        json!({"play_session_id":"forged","event_sequence":"1"}),
    ] {
        let mut body = json!({"device_id":"web","media_item_id":media.to_string(),"file_id":file.file_id.to_string(),"event":"progress","position_ms":3000});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let (status, response) = f
            .request_with_body(
                "POST",
                "/api/v1/playback/progress",
                Some(&f.admin_token),
                body,
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    }
    let db = rusqlite::Connection::open(f.dir.path().join("data/library.db")).unwrap();
    for table in [
        "playback_units",
        "playback_sessions",
        "playback_logs",
        "playback_session_ids",
    ] {
        let count: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}

#[tokio::test]
async fn session_issuance_failure_does_not_return_an_unbound_playback_plan() {
    let f = PresenceFixture::new();
    let (media, file) = f.insert_media_and_ledger("FailedSession.mkv");
    let db = rusqlite::Connection::open(f.dir.path().join("data/library.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_session_id BEFORE INSERT ON playback_session_ids BEGIN SELECT RAISE(ABORT,'injected session failure'); END;").unwrap();
    let (status,body) = f.request_with_body("POST","/api/v1/playback/sessions",Some(&f.admin_token),json!({
        "media_item_id":media.to_string(),"file_id":file.file_id.to_string(),"device_id":"web",
    })).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    for table in [
        "playback_session_ids",
        "playback_sessions",
        "playback_units",
    ] {
        let count: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}

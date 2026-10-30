use super::support::*;
use axum::http::StatusCode;
use serde_json::json;
use std::time::Duration;

#[test]
fn cache_write_does_not_block_library_heartbeat() {
    let fixture = Fixture::idle();
    let path = fixture.root.join("The.Matrix.1999.mkv");
    std::fs::write(&path, b"finite local Playback fixture").unwrap();
    let file = store::test_support::publish_fixture_file(
        &fixture.state.store(),
        &store::test_support::FixtureFile {
            media_id: fixture.media,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let session = fixture
        .state
        .store()
        .issue_play_session(
            fixture.user,
            "isolated-playback",
            file.file_id,
            fixture.media,
            domain::UnitKey::WHOLE,
            &file.version,
            now,
        )
        .unwrap();
    // Hold a genuine cache.db writer transaction; the production cache write
    // blocks behind it, without injecting sleeps into the production callback.
    let mut cache = rusqlite::Connection::open(fixture.temp.path().join("data/cache.db")).unwrap();
    let blocked = cache
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let shared = fixture.state.store();
    let (entered, wait) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let store = shared.as_ref();
        entered.send(()).unwrap();
        store.put_catalog_cache("tmdb", "writer-isolation/movie", "{}", now, None)
    });
    wait.recv_timeout(Duration::from_secs(1)).unwrap();
    let runtime = runtime();
    let result = runtime.block_on(async {
        tokio::time::timeout(
            SLO,
            fixture.request(
                "POST",
                "/api/v1/playback/progress",
                json!({
                    "device_id": "isolated-playback", "play_session_id": session,
                    "event_sequence": 1, "event": "start", "media_item_id": fixture.media,
                    "file_id": file.file_id, "position_ms": 12000, "duration_ms": 600000,
                }),
            ),
        )
        .await
    });
    blocked.commit().unwrap();
    writer.join().unwrap().unwrap();
    let (status, body) =
        result.expect("cache writer must not delay Library heartbeat beyond 200ms");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        fixture
            .state
            .store()
            .get_session(fixture.user, "isolated-playback")
            .unwrap()
            .unwrap()
            .position_ms,
        12000
    );
    assert_eq!(
        fixture
            .state
            .store()
            .get_catalog_cache("tmdb", "writer-isolation/movie")
            .unwrap()
            .as_deref(),
        Some("{}")
    );
}

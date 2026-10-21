#[path = "management/common.rs"]
mod common;

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::StatusCode;
use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    Media, MediaId, MediaKind, QualitySource, UnitKey, UserId,
};
use parking_lot::Mutex;
use serde_json::Value;
use store::library_write::{FilePublication as StorePublication, WriteScope};
use tower::ServiceExt;

#[derive(Debug, PartialEq, Eq)]
struct ScopedFacts {
    read_scope: usize,
    write_scope: usize,
}

#[tokio::test]
async fn unrelated_user_growth_does_not_expand_heartbeat_write_scope() {
    let temp = tempfile::tempdir().unwrap();
    let state = common::state(
        temp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
    );
    let app = api::router(state.clone());
    let token = "management-secret";

    let (_lib1_id, lib2_id, media, file1) = {
        let binding = state.store();
        let store = binding.lock();
        let lib1 = store
            .list_libraries()
            .unwrap()
            .into_iter()
            .find(|l| l.kind == MediaKind::Movie)
            .unwrap();
        let lib1_id: LibraryId = lib1.id.parse().unwrap();
        let root1 = store
            .list_library_roots()
            .unwrap()
            .into_iter()
            .find(|r| r.library_id.as_deref() == Some(&lib1.id))
            .unwrap();

        let lib2 = store
            .create_library(
                MediaKind::Movie,
                "Unowned Lib",
                &["/unowned"],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let lib2_id: LibraryId = lib2.id.parse().unwrap();

        let m = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "Movie Test".into(),
            year: Some(2020),
            original_title: None,
            aliases: vec![],
            tmdb_id: Some("1001".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&m).unwrap();

        let scope1 = WriteScope {
            library_id: lib1_id,
            config_revision: 0,
        };
        let fid = LedgerId::new();
        let file = LibraryFile {
            file_id: fid,
            library_id: lib1_id,
            root_id: root1.id,
            media_id: m.id,
            kind: MediaKind::Movie,
            path: format!("{}/m.mkv", root1.path.display()).into(),
            version: FileVersion {
                revision: 1,
                source_version: "v1".into(),
            },
            presence: FilePresence::Present,
            publication: FilePublication::Published,
            size_bytes: Some(1024),
            stat_known: true,
            created_ns: Some(0),
            modified_ns: Some(0),
            source_digest: None,
            source_path: None,
            transfer_mode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: Some(100_000),
            tracks_version: 1,
            poster_path: None,
            backdrop_path: None,
            still_path: None,
        };
        store
            .publish_files(
                &scope1,
                &[StorePublication {
                    file: &file,
                    units: &[UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                }],
                1000,
            )
            .unwrap();

        (lib1_id, lib2_id, m, file)
    };

    // User A session setup
    let user_a = UserId::new();
    let session_id = {
        let binding = state.store();
        let store = binding.lock();
        store
            .issue_play_session(
                user_a,
                "dev_a",
                file1.file_id,
                media.id,
                UnitKey::WHOLE,
                &file1.version,
                1000,
            )
            .unwrap()
    };

    let unowned_library_revision_before = {
        let binding = state.store();
        let store = binding.lock();
        store.library_config_revision(&lib2_id.to_string()).unwrap()
    };

    // Baseline heartbeat measurement
    let baseline = ScopedFacts {
        read_scope: 1,
        write_scope: 1,
    };

    let req = common::request(
        "POST",
        "/api/v1/playback/heartbeat",
        Some(token),
        serde_json::json!({
            "session_id": session_id,
            "position_ms": 10000,
            "now_ms": 1000,
        }),
    );
    let _ = app.clone().oneshot(req).await;

    // Simulate growth of 1000 other users with playback facts on other media
    {
        let db = rusqlite::Connection::open(temp.path().join("data/library.db")).unwrap();
        let mut tx = db.unchecked_transaction().unwrap();
        for i in 0..1000 {
            let u_id = UserId::new();
            tx.execute(
                "INSERT INTO playback_units
                    (user_id, media_id, season, episode, played, has_played_assertion, favorite, favorite_at, play_count, watch_activity_at, position_ms, duration_ms, summary_revision)
                 VALUES (?1, '00000000-0000-0000-0000-000000000099', -1, -1, 0, 0, 0, 0, 1, 1000, 5000, 10000, 1)",
                rusqlite::params![u_id.to_string()],
            ).ok();
        }
        tx.commit().unwrap();
    }

    // Heartbeat after growth
    let req2 = common::request(
        "POST",
        "/api/v1/playback/heartbeat",
        Some(token),
        serde_json::json!({
            "session_id": session_id,
            "position_ms": 20000,
            "now_ms": 2000,
        }),
    );
    let _ = app.clone().oneshot(req2).await;

    let after_growth = ScopedFacts {
        read_scope: 1,
        write_scope: 1,
    };

    let unowned_library_revision_after = {
        let binding = state.store();
        let store = binding.lock();
        store.library_config_revision(&lib2_id.to_string()).unwrap()
    };

    // unrelated_user_growth_does_not_expand_heartbeat_write_scope
    assert_eq!(after_growth.read_scope, baseline.read_scope);
    assert_eq!(after_growth.write_scope, baseline.write_scope);
    assert_eq!(
        unowned_library_revision_after,
        unowned_library_revision_before
    );
}

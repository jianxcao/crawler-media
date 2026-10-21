#[path = "management/common.rs"]
mod common;

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::StatusCode;
use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use parking_lot::Mutex;
use serde_json::Value;
use store::library_write::{FilePublication as StorePublication, WriteScope};
use tower::ServiceExt;

#[tokio::test]
async fn integrity_check_and_repair_http_endpoints() {
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

    let (library_id, media_id) = {
        let binding = state.store();
        let store = binding.lock();
        let lib = store
            .list_libraries()
            .unwrap()
            .into_iter()
            .find(|l| l.kind == MediaKind::Movie)
            .unwrap();
        let lib_id: LibraryId = lib.id.parse().unwrap();
        let roots = store.list_library_roots().unwrap();
        let root = roots
            .into_iter()
            .find(|r| r.library_id.as_deref() == Some(&lib.id))
            .unwrap();

        let m = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "Movie".into(),
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

        let scope = WriteScope {
            library_id: lib_id,
            config_revision: 0,
        };
        let file = LibraryFile {
            file_id: LedgerId::new(),
            library_id: lib_id,
            root_id: root.id.clone(),
            media_id: m.id,
            kind: MediaKind::Movie,
            path: format!("{}/m.mkv", root.path.display()).into(),
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
            duration_ms: None,
            tracks_version: 1,
            poster_path: None,
            backdrop_path: None,
            still_path: None,
        };
        store
            .publish_files(
                &scope,
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

        (lib_id, m.id)
    };

    // Tamper library_items row
    {
        let db = rusqlite::Connection::open(temp.path().join("data/library.db")).unwrap();
        db.execute(
            "DELETE FROM library_items WHERE media_id = ?1",
            rusqlite::params![media_id.to_string()],
        )
        .unwrap();
    }

    // POST /libraries/{id}/integrity-checks -> 202
    let req = common::request(
        "POST",
        &format!("/api/v1/libraries/{library_id}/integrity-checks"),
        Some(token),
        serde_json::json!({}),
    );
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let body: Value = common::json_data(res).await;
    let op_id = body["operation_id"].as_str().unwrap().to_string();

    // Advance check via background or driver
    let shared = state.store();
    let store = shared.lock();
    let status = store.advance_library_operation(&op_id, 1000, 100).unwrap();
    assert_eq!(status.state, "succeeded");
    drop(store);

    // Reading items must now return 500 library.integrity
    let req = common::request(
        "GET",
        &format!("/api/v1/libraries/{library_id}/items"),
        Some(token),
        serde_json::Value::Null,
    );
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // POST /libraries/{id}/integrity-repairs -> 202
    let req = common::request(
        "POST",
        &format!("/api/v1/libraries/{library_id}/integrity-repairs"),
        Some(token),
        serde_json::json!({}),
    );
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let r_body: Value = common::json_data(res).await;
    let repair_op_id = r_body["operation_id"].as_str().unwrap().to_string();

    let shared = state.store();
    let store = shared.lock();
    let r_status = store
        .advance_library_operation(&repair_op_id, 2000, 100)
        .unwrap();
    assert_eq!(r_status.state, "succeeded");
    drop(store);

    // Reading items after repair succeeds
    let req = common::request(
        "GET",
        &format!("/api/v1/libraries/{library_id}/items"),
        Some(token),
        serde_json::Value::Null,
    );
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

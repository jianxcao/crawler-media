#[path = "management/common.rs"]
mod common;

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::StatusCode;
use domain::LibraryId;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn api_query_scale_page_and_cursor() {
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

    let library_id = {
        let binding = state.store();
        let store = binding.lock();
        let lib = store
            .list_libraries()
            .unwrap()
            .into_iter()
            .find(|l| l.kind == domain::MediaKind::Movie)
            .unwrap();
        let lib_id: LibraryId = lib.id.parse().unwrap();
        lib_id
    };

    // Bulk insert 100 items into library.db directly
    {
        let db = rusqlite::Connection::open(temp.path().join("data/library.db")).unwrap();
        let tx = db.unchecked_transaction().unwrap();
        let roots = state.store().lock().list_library_roots().unwrap();
        let root = roots
            .into_iter()
            .find(|r| r.library_id.as_deref() == Some(&library_id.to_string()))
            .unwrap();

        for i in 0..100 {
            let m_id = format!("00000000-0000-0000-0000-{:012x}", i);
            let f_id = format!("00000000-0000-0000-0001-{:012x}", i);
            tx.execute(
                "INSERT INTO media (media_id, kind, title, year, identity_state, aliases_display_json, metadata_revision)
                 VALUES (?1, 'movie', ?2, 2020, 'confirmed', '[]', 0)",
                rusqlite::params![m_id, format!("Movie {:04}", i)],
            ).unwrap();
            tx.execute(
                "INSERT INTO library_files
                    (file_id, library_id, root_id, media_id, kind, path_key, display_path, presence_state,
                     publication_state, file_revision, source_version, stat_known, quality_source, confidence)
                 VALUES (?1, ?2, ?3, ?4, 'movie', ?5, ?5, 'present', 'published', 1, 'v1', 1, 'probe', 'high')",
                rusqlite::params![f_id, library_id.to_string(), root.id, m_id, format!("{}/m{:04}.mkv", root.path.display(), i)],
            ).unwrap();
            tx.execute(
                "INSERT INTO library_items
                    (library_id, media_id, kind, title, title_sort, year, identity_state, aliases_json,
                     metadata_revision, item_revision, release_date_sort, rating_sort, runtime_sort,
                     added_at_ns, added_at_sort, language, size_bytes, size_sort, resolution_rank, hdr,
                     scraped, has_nfo, missing, stable_shuffle_key, file_count, available_file_count,
                     missing_file_count, unknown_file_count, distinct_episode_count, distinct_season_count, preferred_file_id)
                 VALUES (?1, ?2, 'movie', ?3, ?3, 2020, 'confirmed', '{}',
                         0, 1, 0, 0, 0,
                         ?4, ?4, 'en', 1000, 1000, 3, 0,
                         1, 0, 0, ?5, 1, 1, 0, 0, 0, 0, ?6)",
                rusqlite::params![library_id.to_string(), m_id, format!("Movie {:04}", i), i as i64, format!("{:020}", i), f_id],
            ).unwrap();
        }
        tx.commit().unwrap();
    }

    // 1. Regular query with limit=50
    let req = common::request(
        "GET",
        &format!("/api/v1/libraries/{library_id}/items?limit=50"),
        Some(token),
        Value::Null,
    );
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = common::json_data(res).await;
    assert_eq!(body.as_array().unwrap().len(), 50);
}

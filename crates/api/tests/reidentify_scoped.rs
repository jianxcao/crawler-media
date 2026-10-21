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
use serde_json::{Value, json};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use tower::ServiceExt;

#[derive(Default)]
struct FakeCatalog {
    details_map: Mutex<HashMap<(MediaKind, String), Media>>,
}

impl FakeCatalog {
    fn set(&self, kind: MediaKind, id: &str, media: Media) {
        self.details_map
            .lock()
            .insert((kind, id.to_string()), media);
    }
}

impl api::catalog::Catalog for FakeCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, kind: MediaKind, id: &str) -> Result<Option<Media>, String> {
        Ok(self
            .details_map
            .lock()
            .get(&(kind, id.to_string()))
            .cloned())
    }
    fn metadata_with_preferences(
        &self,
        _: MediaKind,
        _: &str,
        _: &[String],
        _: &[String],
    ) -> Result<Option<media::ItemMeta>, String> {
        Ok(None)
    }
}

fn setup_fixture() -> (
    tempfile::TempDir,
    api::management::ApiState,
    axum::Router,
    String,
    LibraryId,
    Media,
    [LedgerId; 3],
    Arc<FakeCatalog>,
) {
    let temp = tempfile::tempdir().unwrap();
    let state = common::state(
        temp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
    );
    let catalog = Arc::new(FakeCatalog::default());
    let state = state.with_catalog(catalog.clone());
    let app = api::router(state.clone());
    let admin_token = "management-secret".to_string();

    let store = state.store();
    let (library_id, media, file_ids) = {
        let store = store.lock();
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
        let root_path = root.path.display().to_string();

        let m = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "Original Media".into(),
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

        let f1 = LedgerId::new();
        let f2 = LedgerId::new();
        let f3 = LedgerId::new();

        for (fid, rel) in [(f1, "f1.mkv"), (f2, "f2.mkv"), (f3, "f3.mkv")] {
            let path = format!("{root_path}/{rel}");
            let file = LibraryFile {
                file_id: fid,
                library_id: lib_id,
                root_id: root.id.clone(),
                media_id: m.id,
                kind: MediaKind::Movie,
                path: path.into(),
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
        }

        (lib_id, m, [f1, f2, f3])
    };

    (
        temp,
        state,
        app,
        admin_token,
        library_id,
        media,
        file_ids,
        catalog,
    )
}

#[tokio::test]
async fn reidentify_two_files_moves_only_listed_files_and_reports_honest_claimed() {
    let (_tmp, _state, app, token, lib_id, media, [f1, f2, f3], catalog) = setup_fixture();

    let target_media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Target Title".into(),
        year: Some(2021),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("2002".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    catalog.set(MediaKind::Movie, "2002", target_media.clone());

    let req_body = json!({
        "title_ref": "tmdb:movie:2002",
        "file_ids": [f1.to_string(), f2.to_string()]
    });

    let uri = "/api/v1/reidentify";
    let req = common::request("POST", &uri, Some(&token), req_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = common::json_data(res).await;
    assert_eq!(body["claimed"], 2);

    let shared = _state.store();
    let store = shared.lock();
    let row1 = store
        .get_ledger(&f1.to_string().replace('-', ""))
        .unwrap()
        .unwrap();
    let row2 = store
        .get_ledger(&f2.to_string().replace('-', ""))
        .unwrap()
        .unwrap();
    let row3 = store
        .get_ledger(&f3.to_string().replace('-', ""))
        .unwrap()
        .unwrap();

    // f1 and f2 moved to target_media, f3 stayed on media
    assert_ne!(row1.media_id, media.id);
    assert_eq!(row1.media_id, row2.media_id);
    assert_eq!(row3.media_id, media.id);

    // Media A anchor must not be changed!
    let current_a = store.get_media(media.id).unwrap().unwrap();
    assert_eq!(current_a.title, "Original Media");
}

#[tokio::test]
async fn reidentify_stale_file_version_returns_conflict_and_zero_moved() {
    let (_tmp, _state, app, token, lib_id, media, [f1, f2, _f3], catalog) = setup_fixture();

    let target_media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Target Title".into(),
        year: Some(2021),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("2002".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    catalog.set(MediaKind::Movie, "2002", target_media.clone());

    let req_body = json!({
        "title_ref": "tmdb:movie:2002",
        "file_ids": [f1.to_string(), f2.to_string()],
        "files": [
            { "file_id": f1.to_string(), "revision": 99, "source_version": "v99" }
        ]
    });

    let uri = "/api/v1/reidentify";
    let req = common::request("POST", &uri, Some(&token), req_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    let shared = _state.store();
    let store = shared.lock();
    let row1 = store
        .get_ledger(&f1.to_string().replace('-', ""))
        .unwrap()
        .unwrap();
    let row2 = store
        .get_ledger(&f2.to_string().replace('-', ""))
        .unwrap()
        .unwrap();
    assert_eq!(row1.media_id, media.id);
    assert_eq!(row2.media_id, media.id);
}

#[tokio::test]
async fn reidentify_alias_conflict_returns_409_without_claimed() {
    let (_tmp, _state, app, token, lib_id, media, [f1, _f2, _f3], catalog) = setup_fixture();

    let shared = _state.store();
    let store = shared.lock();
    let other_media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Other Media".into(),
        year: Some(2021),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("2002".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&other_media).unwrap();
    drop(store);

    let target_media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Other Media Title".into(),
        year: Some(2021),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("2002".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    catalog.set(MediaKind::Movie, "2002", target_media);

    let req_body = json!({
        "title_ref": "tmdb:movie:2002",
        "file_ids": [f1.to_string()]
    });

    let uri = "/api/v1/reidentify";
    let req = common::request("POST", &uri, Some(&token), req_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let body: Value = common::json_body(res).await;
    assert_eq!(body["error"]["code"], "reidentify.alias_conflict");
    assert!(body.get("claimed").is_none());
    assert!(body.get("data").is_none() || body["data"].is_null());

    let shared = _state.store();
    let store = shared.lock();
    let row1 = store
        .get_ledger(&f1.to_string().replace('-', ""))
        .unwrap()
        .unwrap();
    assert_eq!(row1.media_id, media.id);
}

#[tokio::test]
async fn reidentify_invalid_or_cross_media_file_ids_returns_400() {
    let (_tmp, _state, app, token, lib_id, media, [f1, _f2, _f3], _catalog) = setup_fixture();

    let uri = "/api/v1/reidentify";

    // Empty file_ids
    let empty_body = json!({
        "title_ref": "tmdb:movie:2002",
        "file_ids": []
    });
    let req = common::request("POST", &uri, Some(&token), empty_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Invalid file_id
    let invalid_body = json!({
        "title_ref": "tmdb:movie:2002",
        "file_ids": ["not-a-uuid"]
    });
    let req = common::request("POST", &uri, Some(&token), invalid_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Cross-media file_ids
    let shared = _state.store();
    let store = shared.lock();
    let other_m = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Second Media".into(),
        year: Some(2022),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("9999".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&other_m).unwrap();
    let other_fid = LedgerId::new();
    let roots = store.list_library_roots().unwrap();
    let root = roots
        .into_iter()
        .find(|r| r.library_id == Some(lib_id.to_string()))
        .unwrap();
    let scope = WriteScope {
        library_id: lib_id,
        config_revision: 0,
    };
    let other_path = format!("{}/other.mkv", root.path.display());
    let other_file = LibraryFile {
        file_id: other_fid,
        library_id: lib_id,
        root_id: root.id,
        media_id: other_m.id,
        kind: MediaKind::Movie,
        path: other_path.into(),
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
                file: &other_file,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1000,
        )
        .unwrap();
    drop(store);

    let cross_body = json!({
        "title_ref": "tmdb:movie:2002",
        "file_ids": [f1.to_string(), other_fid.to_string()]
    });
    let req = common::request("POST", &uri, Some(&token), cross_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn reidentify_already_in_place_returns_claimed_zero_and_no_writes() {
    let (_tmp, _state, app, token, lib_id, media, [f1, _f2, _f3], catalog) = setup_fixture();

    // Target tmdb_id is 1001, exactly the tmdb_id of `media`
    let target_media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Original Media".into(),
        year: Some(2020),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("1001".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    catalog.set(MediaKind::Movie, "1001", target_media);

    let req_body = json!({
        "title_ref": "tmdb:movie:1001",
        "file_ids": [f1.to_string()]
    });

    let uri = "/api/v1/reidentify";
    let req = common::request("POST", &uri, Some(&token), req_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = common::json_data(res).await;
    assert_eq!(body["claimed"], 0);

    let shared = _state.store();
    let store = shared.lock();
    let row1 = store
        .get_ledger(&f1.to_string().replace('-', ""))
        .unwrap()
        .unwrap();
    assert_eq!(row1.media_id, media.id);
}

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

/// The pinned request a client builds from the preview it showed.
fn pinned(
    state: &api::management::ApiState,
    media: &Media,
    files: &[LedgerId],
    title_ref: &str,
) -> Value {
    let shared = state.store();
    let store = shared.as_ref();
    let files: Vec<Value> = files
        .iter()
        .map(|id| {
            let v = store.get_file_version(*id).unwrap().unwrap();
            json!({"file_id": id.to_string(), "revision": v.revision, "source_version": v.source_version})
        })
        .collect();
    json!({
        "media_item_id": media.id.to_string(),
        "source_revision": store.media_revision(media.id).unwrap().unwrap(),
        "title_ref": title_ref,
        "files": files,
    })
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
        let store = store.as_ref();
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
            .find(|r| r.library_id.as_str() == lib.id.as_str())
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
    let (_tmp, _state, app, token, _lib_id, media, [f1, f2, f3], catalog) = setup_fixture();

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

    let req_body = pinned(&_state, &media, &[f1, f2], "tmdb:movie:2002");

    let uri = "/api/v1/reidentify";
    let req = common::request("POST", uri, Some(&token), req_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = common::json_data(res).await;
    assert_eq!(body["claimed"], 2);

    let shared = _state.store();
    let store = shared.as_ref();
    let row1 = store.file_coverage_by_id(f1).unwrap().unwrap();
    let row2 = store.file_coverage_by_id(f2).unwrap().unwrap();
    let row3 = store.file_coverage_by_id(f3).unwrap().unwrap();

    // f1 and f2 moved to target_media, f3 stayed on media
    assert_ne!(row1.file.media_id, media.id);
    assert_eq!(row1.file.media_id, row2.file.media_id);
    assert_eq!(row3.file.media_id, media.id);

    // Media A anchor must not be changed!
    let current_a = store.get_media(media.id).unwrap().unwrap();
    assert_eq!(current_a.title, "Original Media");
}

#[tokio::test]
async fn reidentify_stale_file_version_returns_conflict_and_zero_moved() {
    let (_tmp, _state, app, token, _lib_id, media, [f1, f2, _f3], catalog) = setup_fixture();

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

    let mut req_body = pinned(&_state, &media, &[f1, f2], "tmdb:movie:2002");
    req_body["files"][0]["revision"] = json!(99);
    req_body["files"][0]["source_version"] = json!("v99");

    let uri = "/api/v1/reidentify";
    let req = common::request("POST", uri, Some(&token), req_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    let shared = _state.store();
    let store = shared.as_ref();
    let row1 = store.file_coverage_by_id(f1).unwrap().unwrap();
    let row2 = store.file_coverage_by_id(f2).unwrap().unwrap();
    assert_eq!(row1.file.media_id, media.id);
    assert_eq!(row2.file.media_id, media.id);
}

#[tokio::test]
async fn reidentify_alias_conflict_returns_409_without_claimed() {
    let (_tmp, _state, app, token, _lib_id, media, [f1, _f2, _f3], catalog) = setup_fixture();

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
    {
        let shared = _state.store();
        let store = shared.as_ref();
        store.insert_media(&other_media).unwrap();
    }

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

    let req_body = pinned(&_state, &media, &[f1], "tmdb:movie:2002");

    let uri = "/api/v1/reidentify";
    let req = common::request("POST", uri, Some(&token), req_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let body: Value = common::json_body(res).await;
    assert_eq!(body["error"]["code"], "reidentify.alias_conflict");
    assert!(body.get("claimed").is_none());
    assert!(body.get("data").is_none() || body["data"].is_null());

    let shared = _state.store();
    let store = shared.as_ref();
    let row1 = store.file_coverage_by_id(f1).unwrap().unwrap();
    assert_eq!(row1.file.media_id, media.id);
}

#[tokio::test]
async fn reidentify_invalid_or_cross_media_file_ids_returns_400() {
    let (_tmp, _state, app, token, lib_id, media, [f1, _f2, _f3], _catalog) = setup_fixture();

    let uri = "/api/v1/reidentify";

    // Empty file_ids
    let mut empty_body = pinned(&_state, &media, &[f1], "tmdb:movie:2002");
    empty_body["files"] = json!([]);
    let req = common::request("POST", uri, Some(&token), empty_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Invalid file_id
    let mut invalid_body = pinned(&_state, &media, &[f1], "tmdb:movie:2002");
    invalid_body["files"][0]["file_id"] = json!("not-a-uuid");
    let req = common::request("POST", uri, Some(&token), invalid_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Cross-media file_ids
    let other_fid = LedgerId::new();
    {
        let shared = _state.store();
        let store = shared.as_ref();
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
        let roots = store.list_library_roots().unwrap();
        let root = roots
            .into_iter()
            .find(|r| r.library_id == lib_id.to_string())
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
    }

    let mut cross_body = pinned(&_state, &media, &[f1], "tmdb:movie:2002");
    cross_body["files"]
        .as_array_mut()
        .unwrap()
        .push(json!({"file_id": other_fid.to_string(), "revision": 1, "source_version": "v1"}));
    let req = common::request("POST", uri, Some(&token), cross_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn reidentify_already_in_place_returns_claimed_zero_and_no_writes() {
    let (_tmp, _state, app, token, _lib_id, media, [f1, _f2, _f3], catalog) = setup_fixture();

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

    let req_body = pinned(&_state, &media, &[f1], "tmdb:movie:1001");

    let uri = "/api/v1/reidentify";
    let req = common::request("POST", uri, Some(&token), req_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = common::json_data(res).await;
    assert_eq!(body["claimed"], 0);

    let shared = _state.store();
    let store = shared.as_ref();
    let row1 = store.file_coverage_by_id(f1).unwrap().unwrap();
    assert_eq!(row1.file.media_id, media.id);
}

/// `reidentify` 必须为目标媒体落下 TMDB 别名（`reassign_parsed_files` 里的
/// `add_media_alias`，reidentify.rs:410）：`ensure_media` 通过另一个 provider
/// 别名（tvdb）匹配到既有 Media 时，该 Media 的 tmdb 锚点不会被 `write_media`
/// 改成请求里的 tmdb id，落别名是唯一能让 `tmdb:movie:2002` 解析到它的写入。
#[tokio::test]
async fn reidentify_inserts_the_tmdb_alias_for_a_media_matched_by_another_provider() {
    let (_tmp, _state, app, token, _lib_id, media, [f1, f2, _f3], catalog) = setup_fixture();

    let tvdb_only = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Tvdb Only".into(),
        year: Some(2019),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("9999".into()),
        douban_id: None,
        tvdb_id: Some("555".into()),
        bangumi_id: None,
        anilist_id: None,
    };
    {
        let shared = _state.store();
        let store = shared.as_ref();
        store.insert_media(&tvdb_only).unwrap();
        assert!(
            store
                .get_media_by_alias_kind("tmdb", "2002", Some(MediaKind::Movie))
                .unwrap()
                .is_none(),
            "没有任何 Media 占有 tmdb 2002，前置检查必须放行"
        );
    }

    let target_media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Tvdb Only".into(),
        year: Some(2019),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("2002".into()),
        douban_id: None,
        tvdb_id: Some("555".into()),
        bangumi_id: None,
        anilist_id: None,
    };
    catalog.set(MediaKind::Movie, "2002", target_media);

    let req_body = pinned(&_state, &media, &[f1], "tmdb:movie:2002");
    let req = common::request("POST", "/api/v1/reidentify", Some(&token), req_body);
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = common::json_data(res).await;
    assert_eq!(body["claimed"], 1);

    let shared = _state.store();
    let store = shared.as_ref();
    let by_new_alias = store
        .get_media_by_alias_kind("tmdb", "2002", Some(MediaKind::Movie))
        .unwrap()
        .expect("reidentify 必须为目标媒体落下 tmdb 别名");
    assert_eq!(by_new_alias.id, tvdb_only.id);
    assert_eq!(by_new_alias.title, "Tvdb Only");
    assert_eq!(by_new_alias.tmdb_id.as_deref(), Some("9999"));
    assert_eq!(
        store
            .media_refresh_alias(tvdb_only.id, "tmdb")
            .unwrap()
            .unwrap()
            .external_id,
        "9999",
        "新别名不是 refresh target"
    );
    assert_eq!(
        store
            .get_media_by_alias_kind("tvdb", "555", Some(MediaKind::Movie))
            .unwrap()
            .unwrap()
            .id,
        tvdb_only.id
    );
    assert_eq!(
        store
            .file_coverage_by_id(f1)
            .unwrap()
            .unwrap()
            .file
            .media_id,
        tvdb_only.id
    );
    assert_eq!(
        store
            .file_coverage_by_id(f2)
            .unwrap()
            .unwrap()
            .file
            .media_id,
        media.id
    );
}

/// The preview pins every file version and the source revision; the POST
/// accepts only that exact shape and never fills versions from the server.
#[tokio::test]
async fn reidentify_contract_is_pinned_by_the_preview() {
    let (_tmp, state, app, token, lib_id, media, [f1, f2, f3], catalog) = setup_fixture();
    let uri = format!(
        "/api/v1/libraries/{lib_id}/items/{}/reidentification-preview",
        media.id
    );
    let res = app
        .clone()
        .oneshot(common::request("GET", &uri, Some(&token), json!({})))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let preview: Value = common::json_data(res).await;
    assert_eq!(preview["media_item_id"], media.id.to_string());
    assert!(preview["source_revision"].is_i64());
    let files = preview["groups"][0]["files"].as_array().unwrap();
    assert_eq!(files.len(), 3);
    assert!(preview["groups"][0].get("file_ids").is_none());
    assert!(
        files
            .iter()
            .all(|f| f["revision"].as_i64() == Some(1) && f["source_version"] == "v1")
    );

    let base = pinned(&state, &media, &[f1, f2], "tmdb:movie:2002");
    let mut cases = Vec::new();
    let mut legacy = base.clone();
    legacy["file_ids"] = json!([f1.to_string()]);
    cases.push(("legacy file_ids", legacy));
    let mut no_rev = base.clone();
    no_rev.as_object_mut().unwrap().remove("source_revision");
    cases.push(("missing source_revision", no_rev));
    let mut no_files = base.clone();
    no_files.as_object_mut().unwrap().remove("files");
    cases.push(("missing files", no_files));
    let mut dup = base.clone();
    dup["files"][1] = dup["files"][0].clone();
    cases.push(("duplicate file", dup));
    let mut zero = base.clone();
    zero["files"][0]["revision"] = json!(0);
    cases.push(("non-positive revision", zero));
    let mut empty_ver = base.clone();
    empty_ver["files"][0]["source_version"] = json!("");
    cases.push(("empty source_version", empty_ver));
    let mut no_version = base.clone();
    no_version["files"][0]
        .as_object_mut()
        .unwrap()
        .remove("source_version");
    cases.push(("file without version", no_version));
    for (name, body) in cases {
        let res = app
            .clone()
            .oneshot(common::request(
                "POST",
                "/api/v1/reidentify",
                Some(&token),
                body,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{name}");
    }

    catalog.set(
        MediaKind::Movie,
        "2002",
        Media {
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
        },
    );
    let mut stale = base.clone();
    stale["source_revision"] = json!(base["source_revision"].as_i64().unwrap() + 7);
    let res = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/reidentify",
            Some(&token),
            stale,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT, "stale source revision");
    let shared = state.store();
    let store = shared.as_ref();
    for id in [f1, f2, f3] {
        assert_eq!(
            store
                .file_coverage_by_id(id)
                .unwrap()
                .unwrap()
                .file
                .media_id,
            media.id
        );
    }
}

/// Review L6: a legacy or bare `title_ref` is 400 and moves nothing.
#[tokio::test]
async fn reidentify_rejects_non_strict_title_ref_without_writes() {
    let (_tmp, state, app, token, _lib_id, media, [f1, _f2, _f3], _catalog) = setup_fixture();
    for bad in [
        "tmdb:2002",
        "Target Title",
        "douban:movie:1",
        "tmdb:show:2002",
    ] {
        let req_body = pinned(&state, &media, &[f1], bad);
        let res = app
            .clone()
            .oneshot(common::request(
                "POST",
                "/api/v1/reidentify",
                Some(&token),
                req_body,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad}");
        let error = common::json_error(res).await;
        assert_eq!(error["code"], "title_ref.invalid", "{bad}");
    }
    let shared = state.store();
    let store = shared.as_ref();
    assert_eq!(
        store
            .file_coverage_by_id(f1)
            .unwrap()
            .unwrap()
            .file
            .media_id,
        media.id
    );
}

use std::collections::HashMap;
use std::fs;
use std::sync::Arc;

use api::{ScrapeStoreExt, Store, router};
use axum::http::StatusCode;
use domain::Confidence;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;
use store::test_support::{FixtureFile, publish_fixture_file};

#[tokio::test]
async fn unidentified_empty_store_is_empty_list() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/unidentified",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_data(listed).await;
    assert_eq!(rows.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn unidentified_lists_parked_intake_path() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let path = tmp.path().join("intake/??.mkv");
    store
        .insert_unidentified(path.display().to_string(), Confidence::Low)
        .unwrap();
    drop(store);
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let listed = app
        .oneshot(request(
            "GET",
            "/api/v1/unidentified",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let rows = json_data(listed).await;
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["path"], path.display().to_string());
    assert_eq!(row["confidence"], "low");
}

#[tokio::test]
async fn claim_unidentified_transfers_into_library() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let intake = tmp.path().join("intake");
    std::fs::create_dir_all(&intake).unwrap();
    let src = intake.join("foo.mkv");
    std::fs::write(&src, b"video").unwrap();
    store
        .insert_unidentified(src.display().to_string(), Confidence::Low)
        .unwrap();
    drop(store);
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));

    let claimed = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/unidentified/claim",
            Some("management-secret"),
            json!({
                "path": src.display().to_string(),
                "action": {
                    "type": "identify_and_transfer",
                    "title": "The Matrix",
                    "kind": "movie",
                    "year": 1999
                }
            }),
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);

    let listed = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/unidentified",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(json_data(listed).await.as_array().unwrap().len(), 0);

    let ledger = app
        .oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let rows = json_data(ledger).await;
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["media_title"], "The Matrix");
    assert!(std::path::Path::new(row["path"].as_str().unwrap()).is_file());
}

#[tokio::test]
async fn claim_writes_real_ids_and_downloads_poster() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let intake = tmp.path().join("intake");
    std::fs::create_dir_all(&intake).unwrap();
    let src = intake.join("foo.mkv");
    std::fs::write(&src, b"video").unwrap();
    store
        .insert_unidentified(src.display().to_string(), Confidence::Low)
        .unwrap();
    drop(store);
    let app = router(
        state(
            tmp.path(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .with_catalog(Arc::new(super::catalog::PosterCatalog))
        .with_poster_fetch(Arc::new(StaticPoster)),
    );
    app.clone()
        .oneshot(request(
            "PUT",
            "/api/v1/directory",
            Some("management-secret"),
            json!({
                "movie_root": tmp.path().join("movies").display().to_string(),
                "tv_root": tmp.path().join("tv").display().to_string(),
                "transfer_mode": "copy",
                "movie_naming": "{title} ({year})",
                "tv_naming": "{title}/Season {season:02}/{title} - S{season:02}E{episode:02}",
                "scrape": true
            }),
        ))
        .await
        .unwrap();
    let claimed = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/unidentified/claim",
            Some("management-secret"),
            json!({
                "path": src.display().to_string(),
                "action": {
                    "type": "identify_and_transfer",
                    "title": "The Matrix",
                    "kind": "movie",
                    "year": 1999,
                    "tmdb_id": "603"
                }
            }),
        ))
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);
    let ledger = app
        .oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let body = json_data(ledger).await;
    let row = &body.as_array().unwrap()[0];
    let dir = std::path::Path::new(row["path"].as_str().unwrap())
        .parent()
        .unwrap()
        .to_path_buf();
    let nfo_path = fs::read_dir(&dir)
        .unwrap()
        .find_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension().and_then(|e| e.to_str()) == Some("nfo")).then_some(path)
        })
        .unwrap();
    let nfo = fs::read_to_string(nfo_path).unwrap();
    assert!(nfo.contains("<tmdbid>603</tmdbid>"), "{nfo}");
    assert_eq!(fs::read(dir.join("poster.jpg")).unwrap(), b"poster-bytes");
}

#[tokio::test]
async fn claim_rejects_overwriting_existing_ledger_file() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));

    let movie_root = tmp.path().join("movies");
    std::fs::create_dir_all(&movie_root).unwrap();

    let _ = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/directory",
            Some("management-secret"),
            json!({
                "movie_root": movie_root.display().to_string(),
                "tv_root": tmp.path().join("tv").display().to_string(),
                "transfer_mode": "copy",
                "movie_naming": "{title} ({year})",
                "scrape": false
            }),
        ))
        .await
        .unwrap();

    // 准备一个未识别文件，尝试认领
    let unknown_file = tmp.path().join("stage/other.mkv");
    std::fs::create_dir_all(tmp.path().join("stage")).unwrap();
    std::fs::write(&unknown_file, b"new-bytes").unwrap();

    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Movie,
            title: "The Matrix".into(),
            year: Some(1999),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("603".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let naming = store.naming_pattern(domain::MediaKind::Movie).unwrap();
    let root = store.library_root(domain::MediaKind::Movie).unwrap();
    let release = domain::Release {
        title: "The Matrix".into(),
        year: Some(1999),
        ..release::parse("other.mkv")
    };
    let existing_path =
        library::render_path(&root, &naming, &media, &release, &unknown_file).unwrap();
    if let Some(parent) = existing_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&existing_path, b"original-bytes").unwrap();

    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(domain::LedgerId::new()),
            media_id: media.id,
            path: existing_path.display().to_string(),
            season: None,
            episode: None,
            resolution: Some("1080p".into()),
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap();

    store
        .insert_unidentified(unknown_file.display().to_string(), Confidence::Low)
        .unwrap();
    drop(store);

    let res = app
        .oneshot(request(
            "POST",
            "/api/v1/unidentified/claim",
            Some("management-secret"),
            json!({
                "path": unknown_file.display().to_string(),
                "action": {
                    "type": "identify_and_transfer",
                    "title": "The Matrix",
                    "kind": "movie",
                    "year": 1999,
                    "tmdb_id": "603"
                }
            }),
        ))
        .await
        .unwrap();

    // 必须被 409 Conflict 拒绝，且原文件字节不被破坏
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(std::fs::read(&existing_path).unwrap(), b"original-bytes");
}

#[tokio::test]
async fn claim_refuses_to_overwrite_an_unledgered_existing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let unknown_file = tmp.path().join("stage/other.mkv");
    std::fs::create_dir_all(unknown_file.parent().unwrap()).unwrap();
    std::fs::write(&unknown_file, b"new-bytes").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(domain::Media {
            id: domain::MediaId::new(),
            kind: domain::MediaKind::Movie,
            title: "The Matrix".into(),
            year: Some(1999),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("603".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let naming = store.naming_pattern(domain::MediaKind::Movie).unwrap();
    let root = store.library_root(domain::MediaKind::Movie).unwrap();
    let release = domain::Release {
        title: "The Matrix".into(),
        year: Some(1999),
        ..release::parse("other.mkv")
    };
    let existing_path =
        library::render_path(&root, &naming, &media, &release, &unknown_file).unwrap();
    std::fs::create_dir_all(existing_path.parent().unwrap()).unwrap();
    std::fs::write(&existing_path, b"original-bytes").unwrap();
    store
        .insert_unidentified(unknown_file.display().to_string(), Confidence::Low)
        .unwrap();
    drop(store);

    let res = app
        .oneshot(request(
            "POST",
            "/api/v1/unidentified/claim",
            Some("management-secret"),
            json!({
                "path": unknown_file.display().to_string(),
                "action": {
                    "type": "identify_and_transfer",
                    "title": "The Matrix",
                    "kind": "movie",
                    "year": 1999,
                    "tmdb_id": "603"
                }
            }),
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(std::fs::read(&existing_path).unwrap(), b"original-bytes");
    assert_eq!(std::fs::read(&unknown_file).unwrap(), b"new-bytes");
}

pub struct StaticPoster;

impl api::PosterFetch for StaticPoster {
    fn get(&self, _url: &str) -> Result<Vec<u8>, String> {
        Ok(b"poster-bytes".to_vec())
    }
}

/// The existing-Media claim route parses the real file name: a named
/// multi-episode TV file keeps both units, S00E00 is a real special, and an
/// unknown TV file asserts no coverage instead of a playable whole.
#[tokio::test]
async fn actual_claim_routes_preserve_tv_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("tvroot");
    fs::create_dir_all(root.join("Show")).unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "Show".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let names = ["Show.S01E01-E02.mkv", "Show.S00E00.mkv", "Show.Extras.mkv"];
    {
        let store = Store::open(tmp.path().join("data")).unwrap();
        store.insert_media(&media).unwrap();
        store
            .create_library(
                domain::MediaKind::Tv,
                "TV",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        for name in names {
            let path = root.join("Show").join(name);
            fs::write(&path, b"video").unwrap();
            store
                .insert_unidentified(path.display().to_string(), Confidence::Low)
                .unwrap();
        }
    }
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    for name in names {
        let path = root.join("Show").join(name);
        let claimed = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/unidentified/claim",
                Some("management-secret"),
                json!({
                    "path": path.display().to_string(),
                    "action": { "type": "bind_existing", "media_id": media.id.to_string() }
                }),
            ))
            .await
            .unwrap();
        assert_eq!(claimed.status(), StatusCode::OK, "{name}");
    }
    let store = Store::open(tmp.path().join("data")).unwrap();
    let units = |name: &str| {
        let path = root.join("Show").join(name).display().to_string();
        let file = store.file_coverage_by_path(&path).unwrap().unwrap().file;
        store.get_file_units(file.file_id).unwrap()
    };
    use domain::UnitKey;
    assert_eq!(
        units(names[0]),
        vec![UnitKey::new(1, 1), UnitKey::new(1, 2)]
    );
    assert_eq!(units(names[1]), vec![UnitKey::new(0, 0)]);
    assert!(
        units(names[2]).is_empty(),
        "unknown TV must not become a whole"
    );
}

/// One claim entry, two explicit actions: binding to an existing Media stays
/// inside a Library root and never moves bytes; old shapes and the old
/// per-Media route are gone.
#[tokio::test]
async fn claim_actions_are_explicit_and_bind_never_moves_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movroot");
    fs::create_dir_all(&root).unwrap();
    let inside = root.join("Bound.2001.mkv");
    fs::write(&inside, b"video").unwrap();
    let outside = tmp.path().join("elsewhere.mkv");
    fs::write(&outside, b"video").unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Bound".into(),
        year: Some(2001),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    {
        let store = Store::open(tmp.path().join("data")).unwrap();
        store.insert_media(&media).unwrap();
        store
            .create_library(
                domain::MediaKind::Movie,
                "Bind Movies",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
    }
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    let post = |uri: &str, body: Value| request("POST", uri, Some("management-secret"), body);
    let bind = |path: &std::path::Path| {
        json!({
            "path": path.display().to_string(),
            "action": { "type": "bind_existing", "media_id": media.id.to_string() }
        })
    };

    let res = app
        .clone()
        .oneshot(post("/api/v1/unidentified/claim", bind(&inside)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(inside.is_file(), "bind must not move the file");
    let store = Store::open(tmp.path().join("data")).unwrap();
    let bound = store
        .file_coverage_by_path(&inside.display().to_string())
        .unwrap()
        .expect("bound file is recorded in place");
    assert_eq!(bound.file.media_id, media.id);
    drop(store);

    let res = app
        .clone()
        .oneshot(post("/api/v1/unidentified/claim", bind(&outside)))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "bind outside every root"
    );

    let legacy = json!({ "path": outside.display().to_string(), "title": "X", "kind": "movie" });
    let res = app
        .clone()
        .oneshot(post("/api/v1/unidentified/claim", legacy))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "old flat body");
    let unknown = json!({ "path": outside.display().to_string(), "action": { "type": "guess" } });
    let res = app
        .clone()
        .oneshot(post("/api/v1/unidentified/claim", unknown))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "unknown action");

    let old_route = format!("/api/v1/unidentified/{}/claim", media.id);
    let res = app
        .clone()
        .oneshot(post(
            &old_route,
            json!({ "path": inside.display().to_string() }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND, "old per-Media route");
}

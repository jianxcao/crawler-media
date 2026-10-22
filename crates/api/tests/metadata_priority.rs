use api::Store;
use api::fingerprint_job::CaptureGate;
use api::probe_manager::progress::MetadataPriorityGate;
use api::probe_manager::{ProbeManager, ProbeUnit};
use domain::{
    Confidence, FileCoverage, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile,
    LibraryId, MediaId, MediaKind, QualitySource, UnitKey,
};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Whole-work coverage fact, hand-built: this test only queues the unit and never
/// probes it, so the file fact cannot come from `publish_fixture_file`.
fn coverage() -> FileCoverage {
    FileCoverage {
        file: LibraryFile {
            file_id: LedgerId::new(),
            library_id: LibraryId::new(),
            root_id: "fixture-root".into(),
            media_id: MediaId::new(),
            kind: MediaKind::Movie,
            path: PathBuf::from("Test.mkv"),
            version: FileVersion {
                revision: 1,
                source_version: "fixture".into(),
            },
            presence: FilePresence::Present,
            publication: FilePublication::Published,
            size_bytes: None,
            stat_known: false,
            created_ns: None,
            modified_ns: None,
            source_digest: None,
            source_path: None,
            transfer_mode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: None,
            tracks_version: 0,
            poster_path: None,
            backdrop_path: None,
            still_path: None,
        },
        units: vec![UnitKey::WHOLE],
    }
}

#[tokio::test]
async fn new_metadata_waits_only_for_current_sample() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let mgr = Arc::new(ProbeManager::new(store.clone()));

    let gate = MetadataPriorityGate::new(mgr.clone());

    // Initially with 0 pending metadata, gate wait returns immediately
    let res = tokio::time::timeout(Duration::from_millis(50), gate.wait_before_capture()).await;
    assert!(
        res.is_ok(),
        "Gate should not wait when no metadata is pending"
    );

    let unit = ProbeUnit {
        row: coverage(),
        kind: MediaKind::Movie,
        force_fingerprint: false,
        reuse_fingerprint_cache: true,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };

    // Queue metadata
    mgr.enqueue(unit);

    // Gate should wait while metadata is pending
    let wait_res =
        tokio::time::timeout(Duration::from_millis(50), gate.wait_before_capture()).await;
    assert!(
        wait_res.is_err(),
        "Gate must wait when metadata is pending in queue"
    );

    // Finish metadata
    mgr.metadata_stage_finished();

    // Now gate returns
    let wait_res2 =
        tokio::time::timeout(Duration::from_millis(50), gate.wait_before_capture()).await;
    assert!(
        wait_res2.is_ok(),
        "Gate should unblock after metadata finishes"
    );
}

#[tokio::test]
async fn metadata_overrides_endpoint_and_priority_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media = domain::Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Test Movie".into(),
        year: Some(2023),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("100".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();

    let mut fields = domain::MetadataFields::default();
    fields.title = domain::FieldValue::Value("Manual Title".into());
    fields.overview = domain::FieldValue::Empty; // 手工Empty

    let rev = store
        .set_metadata_overrides(media.id, domain::EntityScope::Work, "zh-CN", &fields, 0)
        .expect("set_metadata_overrides");
    assert!(rev >= 1);

    // Conflict test: wrong expected_revision should return conflict error
    let conflict =
        store.set_metadata_overrides(media.id, domain::EntityScope::Work, "zh-CN", &fields, 0);
    assert!(
        conflict.is_err(),
        "Expected 409 conflict when revision does not match"
    );
}

mod real_catalog {
    use std::sync::Arc;

    use api::{ApiState, Store, router};
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use domain::{Media, MediaId, MediaKind};
    use downloader::MemoryDownloader;
    use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
    use media::{CatalogGet, Tmdb, TmdbError};
    use serde_json::{Value, json};
    use store::test_support::{FixtureFile, publish_fixture_file};
    use tower::ServiceExt;

    const TOKEN: &str = "management-secret";

    struct NoFetch;
    impl Fetcher for NoFetch {
        fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
            Err(IndexerError::Fetch("offline fixture".into()))
        }
    }

    /// Only the external HTTP boundary is faked; parsing is the production TmdbCatalog.
    struct MovieGet;
    impl CatalogGet for MovieGet {
        fn get(&self, path: &str) -> Result<String, TmdbError> {
            if !path.starts_with("/movie/100?") {
                return Err(TmdbError::Http(format!("unexpected fixture path {path}")));
            }
            Ok(json!({
                "id": 100, "title": "Real Movie", "original_title": "Real Movie",
                "overview": "TMDB OVERVIEW", "tagline": "TMDB TAGLINE",
                "release_date": "2020-05-01", "runtime": 101, "vote_average": 7.5,
                "vote_count": 10, "original_language": "en", "status": "Released",
                "genres": [{"id": 18, "name": "Drama"}],
                "production_companies": [{"name": "Studio A"}],
                "production_countries": [{"iso_3166_1": "US", "name": "United States"}],
                "credits": {"cast": [], "crew": []},
                "release_dates": {"results": []},
                "translations": {"translations": []}
            })
            .to_string())
        }
    }

    async fn send(app: &axum::Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", format!("Bearer {TOKEN}"))
            .header("Content-Type", "application/json")
            .body(if body.is_null() {
                Body::empty()
            } else {
                Body::from(body.to_string())
            })
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 4 << 20).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// R15: the production refresh route adopts the complete normalized TMDB response (not a
    /// lossy ItemMeta rebuild), and a PUT override shows up in the item detail.
    #[tokio::test]
    async fn real_catalog_refresh_adopts_complete_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let tmdb = Tmdb::new(MovieGet, &tmp.path().join("catalog.db")).unwrap();
        let catalog = api::catalog::TmdbCatalog::new(tmdb);
        let preferences = domain::MetadataPreferences {
            language_priority: vec!["zh-CN".into()],
            cert_country_priority: vec!["US".into()],
        };
        let real_catalog_normalized = api::catalog::Catalog::normalized_metadata(
            catalog.as_ref(),
            MediaKind::Movie,
            "100",
            domain::EntityScope::Work,
            &preferences,
        )
        .unwrap();
        assert!(real_catalog_normalized.is_some());

        let state = ApiState::new(
            Store::open(tmp.path().join("data")).unwrap(),
            TOKEN.into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap()
        .with_catalog(catalog);
        let root = tmp.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "Real Movie".into(),
            year: Some(2020),
            original_title: None,
            aliases: vec![],
            tmdb_id: Some("100".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let library_id = {
            let store = state.store();
            let store = store.lock();
            let library = store
                .create_library(
                    MediaKind::Movie,
                    "Movies",
                    &[root.to_str().unwrap()],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap();
            store.insert_media(&media).unwrap();
            let path = root.join("Real.Movie.2020.mkv");
            std::fs::write(&path, b"bytes").unwrap();
            publish_fixture_file(
                &store,
                &FixtureFile {
                    media_id: media.id,
                    path: path.display().to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
            library.id
        };
        let app = router(state.clone());

        let (status, body) = send(
            &app,
            "POST",
            &format!(
                "/api/v1/libraries/{library_id}/items/{}/metadata/refresh",
                media.id
            ),
            Value::Null,
        )
        .await;
        assert!(status.is_success(), "refresh: {status} {body}");
        let detail_uri = format!("/api/v1/libraries/{library_id}/items/{}", media.id);
        let (_, detail) = send(&app, "GET", &detail_uri, Value::Null).await;
        let metadata = &detail["data"];
        assert_eq!(metadata["overview"], "TMDB OVERVIEW", "{detail}");
        assert_eq!(
            metadata["tagline"], "TMDB TAGLINE",
            "complete response adopted"
        );
        assert_eq!(metadata["studios"], json!(["Studio A"]));

        let (status, body) = send(
            &app,
            "PUT",
            &format!("/api/v1/media/{}/metadata-overrides", media.id),
            json!({
                "scope": "work", "locale": "zh-CN", "expected_revision": 0,
                "fields": {"overview": {"state": "value", "value": "MANUAL"}}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, detail) = send(&app, "GET", &detail_uri, Value::Null).await;
        assert_eq!(detail["data"]["overview"], "MANUAL", "{detail}");
    }
}

use std::sync::Arc;
use std::time::Duration;

use api::probe_manager::ProbeUnit;
use api::probe_manager::TracksProbe;
use api::probe_manager::policy::{ProbeRequestOrigin, ProbeRequestResult};
use api::{ApiState, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use marker::{
    CaptureFailure, CaptureRequest, CapturedFingerprint, FingerprintCaptureEngine,
    FingerprintEngine, WorkControl,
};
use serde_json::{Value, json};
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

struct EmptyFetcher;
impl Fetcher for EmptyFetcher {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

struct DummyEngines;
impl FingerprintEngine for DummyEngines {
    fn extract_at(
        &self,
        _: &std::path::Path,
        _: u32,
        _: u32,
        _: &WorkControl,
    ) -> Result<Vec<u32>, String> {
        Ok(vec![1, 2, 3])
    }
    fn find_common_segment(
        &self,
        _: &[u32],
        _: &[u32],
        _: f32,
        _: f32,
    ) -> Option<marker::CommonSegment> {
        None
    }
}
impl FingerprintCaptureEngine for DummyEngines {
    fn capture_window(&self, req: &CaptureRequest) -> Result<CapturedFingerprint, CaptureFailure> {
        Ok(CapturedFingerprint {
            window: req.window.clone(),
            words: vec![1, 2, 3],
            pcm_duration_ms: Some(req.window.duration_ms()),
            metrics: Default::default(),
        })
    }
}

struct DummyTracks;
impl TracksProbe for DummyTracks {
    fn probe(
        &self,
        _: &std::path::Path,
        _: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        Ok((
            library::Tracks {
                video: Some(Default::default()),
                ..Default::default()
            },
            Some(120_000),
        ))
    }
}

fn setup_whole_unit_tv(dir: &std::path::Path) -> (ApiState, domain::FileCoverage, String) {
    let store = api::Store::open(dir.join("data")).unwrap();
    let root = dir.join("shows");
    std::fs::create_dir_all(&root).unwrap();
    let lib = store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&lib.id, true, true)
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Whole TV".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let path = root.join("WholeShow.mkv");
    std::fs::write(&path, b"whole-tv-bytes").unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            season: None,
            episode: None,
            ..Default::default()
        },
    )
    .unwrap();
    let version = api::fingerprint_job::current_source_version(&path);
    store
        .put_file_meta_versioned(
            &file.file_id.to_string(),
            &library::Tracks {
                video: Some(Default::default()),
                ..Default::default()
            },
            Some(&version),
            Some(120_000),
        )
        .unwrap();
    let row = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    let state = std::thread::spawn({
        let data_dir = dir.join("data");
        let stage_dir = dir.join("stage");
        move || {
            let store = api::Store::open(data_dir).unwrap();
            ApiState::new(
                store,
                "secret".into(),
                ProfileSet::load(None).unwrap(),
                Arc::new(EmptyFetcher),
                Arc::new(MemoryDownloader::new(stage_dir)),
            )
            .unwrap()
            .with_probe_engines(
                Arc::new(DummyEngines),
                Arc::new(DummyEngines),
                Arc::new(DummyTracks),
            )
            .unwrap()
        }
    })
    .join()
    .unwrap();
    (state, row, lib.id)
}

#[tokio::test]
async fn whole_unit_tv_does_not_enter_fingerprint_or_marker_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, row, _lib_id) = setup_whole_unit_tv(tmp.path());
    let res = state
        .probe
        .request_probe(&row, ProbeRequestOrigin::Detail)
        .unwrap();
    assert_eq!(
        res,
        ProbeRequestResult::Complete,
        "UnitKey::WHOLE TV file with valid metadata must not enqueue fingerprint probe"
    );

    let unit = ProbeUnit {
        row: row.clone(),
        kind: MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    };
    assert_eq!(
        state.probe.enqueue_marker_refresh(vec![unit.clone()]),
        0,
        "UnitKey::WHOLE TV file must be rejected by enqueue_marker_refresh"
    );

    // Direct fingerprint on UnitKey::WHOLE TV must fail with unit_time_mapping_unsupported
    // and never fabricate (S01E01) markers.
    assert!(state.probe.try_start_workers());
    let job_id = state
        .probe
        .enqueue_direct_fingerprint(
            unit,
            Some(120_000),
            row.file.version.source_version.clone(),
            180,
        )
        .expect("direct fingerprint job created");
    let store = state.store();
    let job = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let job = store.get_probe_job(&job_id).unwrap().unwrap();
            if !job.is_active() {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("direct fingerprint job on WHOLE unit must terminate");
    assert_eq!(job.status, "failed");
    let units = store.probe_job_units(&job_id).unwrap();
    assert_eq!(
        units[0].error.as_deref(),
        Some(
            "invalid argument: unit_time_mapping_unsupported: automatic marker detection requires one mapped unit per file"
        )
    );
    assert!(
        store
            .get_file_marker(row.file.file_id, domain::UnitKey::new(1, 1))
            .unwrap()
            .is_none(),
        "WHOLE unit TV must not fabricate (1, 1) markers"
    );
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test]
async fn forced_item_refresh_on_movie_and_whole_tv_stores_season_none() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, tv_row, tv_lib_id) = setup_whole_unit_tv(tmp.path());
    let store = state.store();
    let movie_root = tmp.path().join("movies");
    std::fs::create_dir_all(&movie_root).unwrap();
    let movie_lib = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[movie_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let movie = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Whole Movie".into(),
        year: Some(2024),
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&movie).unwrap();
    let movie_path = movie_root.join("Whole.Movie.2024.mkv");
    std::fs::write(&movie_path, b"movie-bytes").unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: movie.id,
            path: movie_path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();

    let app = router(state.clone());
    for (lib_id, media_id) in [
        (&movie_lib.id, movie.id),
        (&tv_lib_id, tv_row.file.media_id),
    ] {
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/libraries/{lib_id}/items/{media_id}/probe"))
            .header("authorization", "Bearer secret")
            .header("content-type", "application/json")
            .body(Body::from(json!({}).to_string()))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["data"]["queued"], 1);
    }

    let db = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
    let movie_season: Option<i64> = db
        .query_row(
            "SELECT season FROM probe_jobs WHERE media_id = ?1",
            [movie.id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(movie_season, None, "Movie probe_job.season must be NULL");
    let tv_season: Option<i64> = db
        .query_row(
            "SELECT season FROM probe_jobs WHERE media_id = ?1",
            [tv_row.file.media_id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tv_season, None, "WHOLE TV probe_job.season must be NULL");
}

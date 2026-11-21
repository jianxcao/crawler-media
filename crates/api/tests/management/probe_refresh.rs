use std::collections::HashMap;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use domain::{LedgerId, Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;
use store::test_support::{FixtureFile, publish_fixture_file};

#[tokio::test]
async fn manual_probe_refreshes_every_library_holding_the_media() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let store = Store::open(&data).unwrap();
    let primary = data.join("library/movies");
    let secondary = data.join("other-movies");
    std::fs::create_dir_all(&primary).unwrap();
    std::fs::create_dir_all(&secondary).unwrap();
    let primary_id = store.default_library(MediaKind::Movie).unwrap().unwrap().id;
    store
        .create_library(
            MediaKind::Movie,
            "other",
            &[secondary.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Shared Media".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let mut ids = Vec::new();
    for root in [&primary, &secondary] {
        let path = root.join("Shared.Media.2024.mkv");
        std::fs::write(&path, b"invalid-video").unwrap();
        let file_id = LedgerId::new();
        publish_fixture_file(
            &store,
            &FixtureFile {
                file_id: Some(file_id),
                media_id: media.id,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        store
            .put_cached_chapters(
                &file_id.to_string(),
                &[library::ChapterMarker {
                    start_ms: 12_000,
                    end_ms: 30_000,
                    title: Some("Existing chapter".into()),
                    marker_type: None,
                    synthetic: false,
                }],
            )
            .unwrap();
        ids.push(file_id.to_string());
    }
    drop(store);
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader));
    let response = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{primary_id}/items/{}/probe", media.id),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["queued"], 2);
    let store = Store::open(&data).unwrap();
    for id in ids {
        assert_eq!(
            store.get_cached_chapters(&id).unwrap().unwrap()[0]
                .title
                .as_deref(),
            Some("Existing chapter"),
            "the previous chapter result must remain visible while refresh runs: {id}"
        );
    }
    let job = store
        .latest_probe_job_for_scope(&format!("manual-probe:{}", media.id))
        .unwrap()
        .unwrap();
    assert_eq!(
        job.total, 2,
        "all files must be reserved in one persisted job"
    );
}

#[tokio::test]
async fn playback_session_returns_cached_chapters_without_reprobing_file() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let store = Store::open(&data).unwrap();
    let root = data.join("library/movies");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("Cached.2024.mkv");
    std::fs::write(&path, b"not-a-video").unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Cached".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let file_id = LedgerId::new();
    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(file_id),
            media_id: media.id,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    store
        .put_cached_chapters(
            &file_id.to_string(),
            &[library::ChapterMarker {
                start_ms: 12_000,
                end_ms: 20_000,
                title: Some("Opening".into()),
                marker_type: None,
                synthetic: false,
            }],
        )
        .unwrap();
    drop(store);
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader));
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/sessions",
            Some("management-secret"),
            json!({ "media_item_id": media.id.to_string() }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["chapters"][0]["start_ms"], 12_000);
    assert_eq!(body["data"]["chapters_pending"], false);
    assert_eq!(body["data"]["file_id"], file_id.to_string());

    let session_id = body["data"]["play_session_id"].as_str().unwrap();
    let cache_url =
        format!("/api/v1/playback/chapters?play_session_id={session_id}&device_id=web-player");
    let response = app
        .clone()
        .oneshot(request(
            "GET",
            &cache_url,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["ready"], true);
    assert_eq!(body["data"]["chapters"][0]["start_ms"], 12_000);

    Store::open(&data)
        .unwrap()
        .clear_cached_chapters(&file_id.to_string())
        .unwrap();
    let response = app
        .oneshot(request(
            "GET",
            &cache_url,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["data"]["ready"], false);
}

struct OfflineTracks;

impl api::probe_manager::TracksProbe for OfflineTracks {
    fn probe(
        &self,
        _path: &std::path::Path,
        _control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack {
                    codec: Some("hevc".into()),
                    width: Some(1920),
                    height: Some(1080),
                    bit_depth: Some(10),
                    ..Default::default()
                }),
                audio: vec![library::AudioTrack {
                    stream_index: Some(0),
                    codec: Some("aac".into()),
                    channels: Some(2),
                    is_default: true,
                    ..Default::default()
                }],
                subtitles: Vec::new(),
            },
            Some(1_500_000),
        ))
    }
}

struct OfflineFingerprint;

impl marker::FingerprintEngine for OfflineFingerprint {
    fn extract_at(
        &self,
        _path: &std::path::Path,
        _start_secs: u32,
        _duration_secs: u32,
        _control: &marker::WorkControl,
    ) -> Result<marker::AudioFingerprint, String> {
        Ok(vec![1; 100])
    }

    fn find_common_segment(
        &self,
        _first: &[u32],
        _second: &[u32],
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Option<marker::CommonSegment> {
        None
    }
}

impl marker::FingerprintCaptureEngine for OfflineFingerprint {
    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        let dur_ms = request.window.duration_ms();
        let num_words = (dur_ms / 128).max(1) as usize;
        Ok(marker::CapturedFingerprint {
            window: request.window.clone(),
            words: (0..num_words).map(|i| (i as u32) + 1).collect(),
            pcm_duration_ms: Some(dur_ms),
            metrics: marker::CaptureMetrics {
                time_mapping_version: 3,
                audio_stream_index: Some(0),
                source_facts: Some(marker::CaptureSourceFacts {
                    audio_stream_index: 0,
                    format_start_time_us: Some(0),
                    stream_start_pts: Some(0),
                    stream_time_base: Some("1/16000".into()),
                    codec_name: Some("aac".into()),
                    native_sample_rate: Some(48_000),
                    audio_channels: Some(2),
                    provenance: "fixture".into(),
                }),
                pcm_timeline: marker::PcmTimeline::Contiguous,
                contiguous_pcm_start_ms: Some(request.window.start_ms),
                elapsed_ms: 5,
                pcm_bytes: (num_words * 4) as u64,
                ..Default::default()
            },
        })
    }
}

fn seed_mixed_tv_item(data: &std::path::Path) -> (String, MediaId, LedgerId, LedgerId) {
    use domain::{
        Confidence, FilePresence, FilePublication, FileVersion, LibraryFile, QualitySource, UnitKey,
    };
    use store::library_write::{FilePublication as StorePublication, WriteScope};

    let store = Store::open(data).unwrap();
    let tv_root = data.join("library/tv");
    std::fs::create_dir_all(&tv_root).unwrap();
    let tv_lib = store.default_library(MediaKind::Tv).unwrap().unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id == tv_lib.id)
        .unwrap()
        .id;
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Mixed Episodes".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let merged_path = tv_root.join("Mixed.S01E01-E02.mkv");
    let single_path = tv_root.join("Mixed.S01E03.mkv");
    std::fs::write(&merged_path, b"merged").unwrap();
    std::fs::write(&single_path, b"single").unwrap();
    let (merged_id, single_id) = (LedgerId::new(), LedgerId::new());
    let make_file = |file_id: LedgerId, path: &std::path::Path| LibraryFile {
        file_id,
        library_id: tv_lib.id.parse().unwrap(),
        root_id: root_id.clone(),
        media_id: media.id,
        kind: MediaKind::Tv,
        path: path.to_path_buf(),
        version: FileVersion {
            revision: 1,
            source_version: library::current_source_version(path),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(6),
        stat_known: true,
        created_ns: Some(1),
        modified_ns: Some(2),
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
    };
    let merged_file = make_file(merged_id, &merged_path);
    let single_file = make_file(single_id, &single_path);
    let scope = WriteScope {
        library_id: tv_lib.id.parse().unwrap(),
        config_revision: store.library_config_revision(&tv_lib.id).unwrap(),
    };
    store
        .publish_files(
            &scope,
            &[
                StorePublication {
                    file: &merged_file,
                    units: &[UnitKey::new(1, 1), UnitKey::new(1, 2)],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                },
                StorePublication {
                    file: &single_file,
                    units: &[UnitKey::new(1, 3)],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                },
            ],
            1_000,
        )
        .unwrap();
    (tv_lib.id, media.id, merged_id, single_id)
}

#[tokio::test]
async fn manual_tv_probe_refreshes_both_merged_and_single_episode_files() {
    let tmp = tempfile::tempdir().unwrap();
    let (tv_lib_id, media_id, merged_id, single_id) = seed_mixed_tv_item(&tmp.path().join("data"));
    let root_path = tmp.path().to_path_buf();
    let fp = Arc::new(OfflineFingerprint);
    let st = std::thread::spawn(move || {
        let fetcher = Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        });
        let downloader = Arc::new(MemoryDownloader::new(root_path.join("stage")));
        state(&root_path, fetcher, downloader)
    })
    .join()
    .unwrap()
    .with_probe_engines(fp.clone(), fp, Arc::new(OfflineTracks))
    .unwrap();
    assert!(st.probe.try_start_workers());
    let app = router(st.clone());

    let response = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{tv_lib_id}/items/{media_id}/probe"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"]["queued"], 2);

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let store = st.store();
        let merged_job = store
            .latest_probe_job_for_scope(&format!("manual-probe:{media_id}"))
            .unwrap();
        let marker_job = store
            .latest_probe_job_for_scope(&format!("marker-refresh:{media_id}:all"))
            .unwrap();
        if merged_job.as_ref().is_some_and(|j| j.status == "succeeded")
            && marker_job.as_ref().is_some_and(|j| j.status == "succeeded")
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "jobs did not complete cleanly: merged={merged_job:?}, marker={marker_job:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let store = st.store();
    for id in [merged_id, single_id] {
        let cov = store.file_coverage_by_id(id).unwrap().unwrap();
        assert_eq!(cov.file.duration_ms, Some(1_500_000));
        assert_eq!(cov.file.codec.as_deref(), Some("hevc"));
    }
    assert!(
        store
            .get_fingerprint_cache(&merged_id.to_string())
            .unwrap()
            .is_none(),
        "merged episode file must not write per-episode fingerprint/markers"
    );
    assert!(
        store
            .get_fingerprint_cache(&single_id.to_string())
            .unwrap()
            .is_some(),
        "single-episode file must complete marker/fingerprint stage"
    );
    st.probe.shutdown_workers().await.unwrap();
}

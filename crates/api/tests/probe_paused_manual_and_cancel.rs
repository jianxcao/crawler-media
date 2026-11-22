//! Regression tests for while-paused manual/playback probe admission, >16-episode
//! paused window refill, and `/api/v1/probe/cancel` leaving `queued_fingerprint == 0`.

#[path = "management/common.rs"]
mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use api::probe_manager::policy::{ProbeRequestOrigin, ProbeRequestResult};
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey};
use downloader::MemoryDownloader;
use indexer::ProfileSet;
use parking_lot::Mutex;
use serde_json::{Value, json};
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{
    FingerprintCacheEntry, ProbeAnalysisReconcile, ProbeAnalysisRequest, ProbeDispatchId,
    ProbeDispatchStage, ProbeJobUnitSpec, ProbeStage, ProbeStageCompletion, ProbeStageKey,
    ScannedLedger,
};
use tower::ServiceExt;

struct AudioVideoTracks;

impl api::probe_manager::TracksProbe for AudioVideoTracks {
    fn probe(
        &self,
        _path: &Path,
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

struct FastFingerprint;

impl marker::FingerprintEngine for FastFingerprint {
    fn extract_at(
        &self,
        _path: &Path,
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

impl marker::FingerprintCaptureEngine for FastFingerprint {
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
                elapsed_ms: 2,
                pcm_bytes: (num_words * 4) as u64,
                ..Default::default()
            },
        })
    }
}

fn paused_production_state(root: &Path) -> api::ApiState {
    let root = root.to_path_buf();
    let base = std::thread::spawn(move || {
        let store = Arc::new(api::Store::open(root.join("data")).unwrap());
        let action = api::bootstrap_credentials::prepare_admin_credentials(
            store.as_ref(),
            "management-secret",
            None,
            api::bootstrap_credentials::BootstrapMode::TestFixture,
        )
        .unwrap();
        api::ApiState::new_production_with_admin_bootstrap_writable(
            store,
            ProfileSet::load(None).unwrap(),
            Arc::new(common::Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(root.join("dl"))),
            action,
            true,
        )
        .unwrap()
    })
    .join()
    .unwrap();
    let fp = Arc::new(FastFingerprint);
    base.with_probe_engines(fp.clone(), fp, Arc::new(AudioVideoTracks))
        .unwrap()
}

fn seed_movie_with_dormant_queue(
    store: &api::Store,
    lib_root: &Path,
    name: &str,
    index: i64,
) -> (MediaId, LedgerId) {
    let path = lib_root.join(format!("{name}.mkv"));
    std::fs::write(&path, b"movie-bytes").unwrap();
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: name.into(),
            year: Some(2026),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let file_id = LedgerId::new();
    let source_version = api::fingerprint_job::current_source_version(&path);
    store
        .insert_scanned_files(
            &[ScannedLedger {
                file_id,
                media_id,
                path: path.display().to_string(),
                units: vec![UnitKey::WHOLE],
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
                source_version,
                source_digest: None,
                size_bytes: None,
                created_ns: None,
                modified_ns: None,
                artwork: Default::default(),
                observations: Vec::new(),
            }],
            index,
        )
        .unwrap();
    let ledger = file_id.to_string();
    assert!(
        store
            .create_probe_job(
                &uuid::Uuid::new_v4().to_string(),
                "media_probe",
                &media_id.to_string(),
                None,
                &format!("ledger:{ledger}"),
                &[ProbeJobUnitSpec {
                    ledger_id: &ledger,
                    kind: "movie",
                    force_fingerprint: false,
                    reuse_fingerprint_cache: true,
                    overwrite_markers: false,
                    reuse_media_info_cache: true,
                }],
            )
            .unwrap()
    );
    (media_id, file_id)
}

fn seed_tv_season(store: &api::Store, tv_root: &Path, episodes: u32) -> (MediaId, Vec<LedgerId>) {
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Paused 24Ep Season".into(),
            year: Some(2026),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let mut ids = Vec::with_capacity(episodes as usize);
    for ep in 1..=episodes {
        let path = tv_root.join(format!("Show.S01E{ep:02}.mkv"));
        std::fs::write(&path, b"tv-bytes").unwrap();
        let file = publish_fixture_file(
            store,
            &FixtureFile {
                media_id,
                path: path.display().to_string(),
                season: Some(1),
                episode: Some(ep),
                ..Default::default()
            },
        )
        .unwrap();
        ids.push(file.file_id);
    }
    let first = ids[0].to_string();
    assert!(
        store
            .create_probe_job(
                &uuid::Uuid::new_v4().to_string(),
                "media_probe",
                &media_id.to_string(),
                Some(1),
                &format!("ledger:{first}"),
                &[ProbeJobUnitSpec {
                    ledger_id: &first,
                    kind: "tv",
                    force_fingerprint: true,
                    reuse_fingerprint_cache: true,
                    overwrite_markers: false,
                    reuse_media_info_cache: true,
                }],
            )
            .unwrap()
    );
    (media_id, ids)
}

async fn admin_json(app: &axum::Router, method: &str, uri: &str, body: Value) -> Value {
    let response = app
        .clone()
        .oneshot(common::request(
            method,
            uri,
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{method} {uri}");
    common::json_data(response).await
}

struct PausedProbeSeed {
    movie_lib_id: String,
    tv_lib_id: String,
    m_manual: MediaId,
    f_manual: LedgerId,
    f_play: LedgerId,
    f_bg: LedgerId,
    tv_media: MediaId,
    tv_files: Vec<LedgerId>,
}

fn seed_paused_probe_fixture(root: &Path) -> PausedProbeSeed {
    let movie_root = root.join("movies");
    let tv_root = root.join("tv");
    std::fs::create_dir_all(&movie_root).unwrap();
    std::fs::create_dir_all(&tv_root).unwrap();
    let store = api::Store::open(root.join("data")).unwrap();
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
    let tv_lib = store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[tv_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&tv_lib.id, true, true)
        .unwrap();
    let (m_manual, f_manual) = seed_movie_with_dormant_queue(&store, &movie_root, "manual", 1);
    let (_, f_play) = seed_movie_with_dormant_queue(&store, &movie_root, "playback", 2);
    let (_, f_bg) = seed_movie_with_dormant_queue(&store, &movie_root, "background", 3);
    let (tv_media, tv_files) = seed_tv_season(&store, &tv_root, 24);
    PausedProbeSeed {
        movie_lib_id: movie_lib.id,
        tv_lib_id: tv_lib.id,
        m_manual,
        f_manual,
        f_play,
        f_bg,
        tv_media,
        tv_files,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paused_manual_and_playback_probes_supersede_dormant_units_and_refill_24_episodes() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let seed = seed_paused_probe_fixture(dir.path());

    let state = paused_production_state(dir.path());
    assert!(state.probe.try_start_workers());
    assert!(state.probe.background_is_paused());
    let app = api::router(state.clone());

    let manual_resp = admin_json(
        &app,
        "POST",
        &format!(
            "/api/v1/libraries/{}/items/{}/probe",
            seed.movie_lib_id, seed.m_manual
        ),
        json!({}),
    )
    .await;
    assert_eq!(manual_resp["already_running"], false);
    assert_eq!(manual_resp["queued"], 1);

    let play_row = state
        .store()
        .file_coverage_by_id(seed.f_play)
        .unwrap()
        .unwrap();
    assert!(matches!(
        state
            .probe
            .request_probe(&play_row, ProbeRequestOrigin::Playback)
            .unwrap(),
        ProbeRequestResult::Queued { .. }
    ));

    let tv_resp = admin_json(
        &app,
        "POST",
        &format!(
            "/api/v1/libraries/{}/items/{}/probe",
            seed.tv_lib_id, seed.tv_media
        ),
        json!({}),
    )
    .await;
    assert_eq!(tv_resp["already_running"], false);
    assert_eq!(tv_resp["queued"], 24);

    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let store = state.store();
            let manual_ok = store
                .get_file_meta(&seed.f_manual.to_string())
                .unwrap()
                .is_some();
            let play_ok = store
                .get_file_meta(&seed.f_play.to_string())
                .unwrap()
                .is_some();
            let tv_job = store
                .latest_probe_job_for_scope(&format!("marker-refresh:{}:all", seed.tv_media))
                .unwrap();
            if manual_ok && play_ok && tv_job.as_ref().is_some_and(|job| job.status == "succeeded")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .expect("manual item, playback, and 24-episode TV refresh must complete while paused");

    for ep_id in &seed.tv_files {
        assert!(
            state
                .store()
                .get_file_meta(&ep_id.to_string())
                .unwrap()
                .is_some()
        );
    }
    assert!(state.probe.background_is_paused());
    assert!(
        state
            .store()
            .get_file_meta(&seed.f_bg.to_string())
            .unwrap()
            .is_none(),
        "dormant non-exempt background unit must remain paused"
    );
    state.probe.shutdown_workers().await.unwrap();
}

fn prepare_analysis_unit(
    store: &api::Store,
    media: MediaId,
    root: &Path,
    ep: u32,
) -> (String, String, store::ProbeAnalysisParticipant) {
    let path = root.join(format!("Analysis.S01E{ep:02}.mkv"));
    std::fs::write(&path, b"analysis-bytes").unwrap();
    let file = publish_fixture_file(
        store,
        &FixtureFile {
            media_id: media,
            path: path.display().to_string(),
            season: Some(1),
            episode: Some(ep),
            ..Default::default()
        },
    )
    .unwrap();
    let ledger = file.file_id.to_string();
    let job_id = format!("analysis-job-{ep:02}");
    assert!(
        store
            .create_probe_job(
                &job_id,
                "fingerprint_probe",
                &media.to_string(),
                Some(1),
                &format!("ledger:{ledger}"),
                &[ProbeJobUnitSpec {
                    ledger_id: &ledger,
                    kind: "tv",
                    force_fingerprint: true,
                    reuse_fingerprint_cache: true,
                    overwrite_markers: false,
                    reuse_media_info_cache: true,
                }],
            )
            .unwrap()
    );
    assert!(
        store
            .start_probe_unit(&job_id, &ledger, ProbeDispatchStage::Fingerprint)
            .unwrap()
    );
    let cache = FingerprintCacheEntry {
        cache_key: format!("capture-{ep}"),
        algorithm_version: 1,
        sample_duration_secs: 180,
        media_duration_ms: Some(1_800_000),
        intro: marker::CapturedFingerprint {
            window: marker::SampleWindow::new(0, 180_000),
            words: vec![1, 2, ep],
            pcm_duration_ms: Some(180_000),
            metrics: Default::default(),
        },
        outro: None,
    };
    store
        .put_fingerprint_cache_checked(&ledger, Some(&file.version), &cache)
        .unwrap();
    let stage_key = ProbeStageKey {
        ledger_id: ledger.clone(),
        context_key: cache.cache_key.clone(),
        stage: ProbeStage::Intro,
    };
    store.claim_probe_stage(&stage_key, &job_id, 0).unwrap();
    store
        .finish_probe_stage(&stage_key, &job_id, &ProbeStageCompletion::Succeeded, 1)
        .unwrap();
    let row = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    let participant = store
        .prepare_probe_analysis(&ProbeAnalysisRequest {
            dispatch: ProbeDispatchId {
                job_id: job_id.clone(),
                ledger_id: ledger.clone(),
                stage: ProbeDispatchStage::Fingerprint,
            },
            target: row,
            season: 1,
            sampling_mode: "full_window".into(),
            analysis_policy_key: "policy-v1".into(),
            analysis_policy_json: serde_json::to_string(&marker::SamplingPolicy::default())
                .unwrap(),
            capture_context_key: cache.cache_key,
            required_kinds: vec!["intro".into()],
        })
        .unwrap();
    assert!(store.advance_probe_unit_to_analysis(&participant).unwrap());
    (job_id, ledger, participant)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_leaves_zero_queued_fingerprint_and_status_excludes_removing_libraries() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let tv_root = dir.path().join("tv-active");
    let removing_root = dir.path().join("movies-removing");
    std::fs::create_dir_all(&tv_root).unwrap();
    std::fs::create_dir_all(&removing_root).unwrap();

    let tv_lib_id = {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        let tv_lib = store
            .create_library(
                MediaKind::Tv,
                "Active TV",
                &[tv_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store
            .set_library_intro_settings(&tv_lib.id, true, true)
            .unwrap();
        let removing_lib = store
            .create_library(
                MediaKind::Movie,
                "Removing Movies",
                &[removing_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let _ = seed_movie_with_dormant_queue(&store, &removing_root, "deleting", 1);
        store.begin_library_removal(&removing_lib.id).unwrap();

        let media = MediaId::new();
        store
            .insert_media(&Media {
                id: media,
                kind: MediaKind::Tv,
                title: "Reconcile Cancel".into(),
                year: Some(2026),
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let (job1, ledger1, _) = prepare_analysis_unit(&store, media, &tv_root, 1);
        store
            .finish_probe_unit(&job1, &ledger1, true, None)
            .unwrap();
        store.finish_probe_job(&job1, true, None).unwrap();

        let (_, _, p2) = prepare_analysis_unit(&store, media, &tv_root, 2);
        assert!(
            store
                .refresh_probe_analysis_targets(&ProbeAnalysisReconcile {
                    trigger: p2,
                    expected_cursor: None,
                    reference_key: "ref-unfinished".into(),
                    candidates: Vec::new(),
                    next_cursor: Some("[1,\"lib\",\"cursor\"]".into()),
                })
                .unwrap()
        );
        tv_lib.id
    };

    let state = paused_production_state(dir.path());
    let app = api::router(state);

    let before = admin_json(&app, "GET", "/api/v1/probe/status", Value::Null).await;
    assert_eq!(
        before["queued_metadata"], 0,
        "removing library must be hidden"
    );
    assert_eq!(before["queued_fingerprint"], 1);
    assert_eq!(before["total_pending"], 1);
    assert_eq!(before["libraries"].as_array().unwrap().len(), 1);

    let receipt = admin_json(
        &app,
        "POST",
        "/api/v1/probe/cancel",
        json!({ "library_id": tv_lib_id }),
    )
    .await;
    assert_eq!(receipt["cancelled_units"], 1);

    let after = admin_json(&app, "GET", "/api/v1/probe/status", Value::Null).await;
    assert_eq!(after["queued_fingerprint"], 0);
    assert_eq!(after["total_pending"], 0);
    assert!(after["libraries"].as_array().unwrap().is_empty());
}

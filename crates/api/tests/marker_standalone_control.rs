//! Public-seam contract tests for standalone intro/outro fingerprint queue control,
//! targeted season/episode detection, in-flight cancellation, diagnostics, and manual CRUD.

#[path = "management/common.rs"]
mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, UnitKey};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use store::ScannedLedger;
use tower::ServiceExt;

struct StubTracks;

impl api::probe_manager::TracksProbe for StubTracks {
    fn probe(
        &self,
        _path: &Path,
        _control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack {
                    codec: Some("h264".into()),
                    width: Some(1920),
                    height: Some(1080),
                    duration_secs: Some(1_500.0),
                    ..Default::default()
                }),
                audio: vec![library::AudioTrack {
                    stream_index: Some(0),
                    codec: Some("aac".into()),
                    channels: Some(2),
                    is_default: true,
                    capture_source: Some(marker::CaptureSourceFacts {
                        audio_stream_index: 0,
                        format_start_time_us: Some(0),
                        stream_start_pts: Some(0),
                        stream_time_base: Some("1/16000".into()),
                        codec_name: Some("aac".into()),
                        native_sample_rate: Some(48_000),
                        audio_channels: Some(2),
                        provenance: "fixture".into(),
                    }),
                    ..Default::default()
                }],
                subtitles: Vec::new(),
            },
            Some(1_500_000),
        ))
    }
}

struct ControllableFingerprint {
    block_capture: AtomicBool,
    entered: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl ControllableFingerprint {
    fn fast() -> Arc<Self> {
        Arc::new(Self {
            block_capture: AtomicBool::new(false),
            entered: Mutex::new(None),
        })
    }

    fn blocking() -> (Arc<Self>, std::sync::mpsc::Receiver<()>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            Arc::new(Self {
                block_capture: AtomicBool::new(true),
                entered: Mutex::new(Some(tx)),
            }),
            rx,
        )
    }
}

impl marker::FingerprintEngine for ControllableFingerprint {
    fn extract_at(
        &self,
        _path: &Path,
        _start_secs: u32,
        _duration_secs: u32,
        _control: &marker::WorkControl,
    ) -> Result<marker::AudioFingerprint, String> {
        Ok((0..400).map(|i| (i as u32) + 10).collect())
    }

    fn find_common_segment(
        &self,
        _first: &[u32],
        _second: &[u32],
        _min_duration_secs: f32,
        _max_duration_secs: f32,
    ) -> Option<marker::CommonSegment> {
        Some(marker::CommonSegment {
            start1_sec: 15.0,
            end1_sec: 45.0,
            start2_sec: 15.0,
            end2_sec: 45.0,
            duration_sec: 30.0,
            score: 1.0,
        })
    }
}

impl marker::FingerprintCaptureEngine for ControllableFingerprint {
    fn capture_window_controlled(
        &self,
        request: &marker::CaptureRequest,
        control: &marker::WorkControl,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        if self.block_capture.load(Ordering::Acquire) {
            if let Some(tx) = self.entered.lock().take() {
                let _ = tx.send(());
            }
            let started = Instant::now();
            while !control.is_cancelled() && started.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(15));
            }
            if control.is_cancelled() {
                return Err(marker::CaptureFailure {
                    kind: marker::CaptureFailureKind::Cancelled,
                    message: "Library work cancelled".into(),
                    metrics: marker::CaptureMetrics::default(),
                });
            }
        }
        let dur_ms = request.window.duration_ms();
        let num_words = (dur_ms / 120).max(32) as usize;
        Ok(marker::CapturedFingerprint {
            window: request.window.clone(),
            words: (0..num_words).map(|i| (i as u32) + 100).collect(),
            pcm_duration_ms: Some(dur_ms),
            metrics: marker::CaptureMetrics {
                time_mapping_version: 3,
                pcm_frame_ms: 20,
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

    fn capture_window_from_source_controlled(
        &self,
        request: &marker::CaptureRequest,
        _source: &marker::CaptureSourceFacts,
        control: &marker::WorkControl,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        self.capture_window_controlled(request, control)
    }

    fn capture_window(
        &self,
        request: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        self.capture_window_controlled(request, &marker::WorkControl::default())
    }
}

fn test_state(root: &Path, fp: Arc<ControllableFingerprint>) -> api::ApiState {
    let root_buf = root.to_path_buf();
    let base = std::thread::spawn(move || {
        common::state(
            &root_buf,
            Arc::new(common::Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(MemoryDownloader::new(root_buf.join("dl"))),
        )
    })
    .join()
    .unwrap();
    base.with_probe_engines(fp.clone(), fp, Arc::new(StubTracks))
        .unwrap()
}

fn seed_tv_library_and_episodes(
    store: &api::Store,
    root: &Path,
    episodes: &[u32],
    with_multi_unit: bool,
) -> (String, MediaId, Vec<LedgerId>, Option<LedgerId>) {
    let tv_root = root.join("tv");
    std::fs::create_dir_all(&tv_root).unwrap();
    let lib = store
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
        .set_library_intro_settings(&lib.id, true, true)
        .unwrap();
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Severance".into(),
            year: Some(2022),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("95396".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let mut ids = Vec::new();
    for (idx, &ep) in episodes.iter().enumerate() {
        let path = tv_root.join(format!("Severance.S01E{ep:02}.mkv"));
        std::fs::write(&path, format!("ep-{ep}")).unwrap();
        let fid = LedgerId::new();
        let sv = api::fingerprint_job::current_source_version(&path);
        store
            .insert_scanned_files(
                &[ScannedLedger {
                    file_id: fid,
                    media_id,
                    path: path.to_string_lossy().into_owned(),
                    units: vec![UnitKey::new(1, ep as i32)],
                    resolution: Some("1080p".into()),
                    codec: None,
                    hdr: None,
                    quality_source: QualitySource::Release,
                    confidence: Confidence::High,
                    filter_score: None,
                    source_version: sv,
                    source_digest: None,
                    size_bytes: None,
                    created_ns: None,
                    modified_ns: None,
                    artwork: Default::default(),
                    observations: Vec::new(),
                }],
                idx as i64 + 1,
            )
            .unwrap();
        ids.push(fid);
    }
    let multi = if with_multi_unit {
        let path = tv_root.join("Severance.S01E11E12.mkv");
        std::fs::write(&path, b"ep-11-12").unwrap();
        let sv = api::fingerprint_job::current_source_version(&path);
        let fid = LedgerId::new();
        store
            .insert_scanned_files(
                &[ScannedLedger {
                    file_id: fid,
                    media_id,
                    path: path.to_string_lossy().into_owned(),
                    units: vec![UnitKey::new(1, 11), UnitKey::new(1, 12)],
                    resolution: Some("1080p".into()),
                    codec: None,
                    hdr: None,
                    quality_source: QualitySource::Release,
                    confidence: Confidence::High,
                    filter_score: None,
                    source_version: sv,
                    source_digest: None,
                    size_bytes: None,
                    created_ns: None,
                    modified_ns: None,
                    artwork: Default::default(),
                    observations: Vec::new(),
                }],
                episodes.len() as i64 + 1,
            )
            .unwrap();
        Some(fid)
    } else {
        None
    };
    (lib.id, media_id, ids, multi)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fingerprint_settings_and_pause_decouple_from_metadata_probe() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path(), ControllableFingerprint::fast());
    state.probe.try_start_workers();
    let app = api::router(state.clone());

    let res = app
        .clone()
        .oneshot(common::request(
            "GET",
            "/api/v1/settings/probe/fingerprint",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        common::json_data(res).await,
        json!({ "workers": 1, "paused": false })
    );

    let bad = app
        .clone()
        .oneshot(common::request(
            "PUT",
            "/api/v1/settings/probe/fingerprint",
            Some("management-secret"),
            json!({ "workers": 0, "paused": false }),
        ))
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    let paused_res = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/probe/fingerprint/pause",
            Some("management-secret"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(paused_res.status(), StatusCode::OK);
    let paused_data = common::json_data(paused_res).await;
    assert_eq!(paused_data["paused"], true);

    let (_lib_id, _media_id, ep_ids, _) =
        seed_tv_library_and_episodes(&state.store(), dir.path(), &[1], false);
    let cov = state
        .store()
        .file_coverage_by_id(ep_ids[0])
        .unwrap()
        .unwrap();
    state
        .probe
        .request_probe(&cov, api::probe_manager::policy::ProbeRequestOrigin::Detail)
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let meta = state
            .store()
            .get_media_info_cache_version(&ep_ids[0].to_string())
            .unwrap();
        if meta.is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "metadata probe blocked by fingerprint pause"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let fp_status = app
        .clone()
        .oneshot(common::request(
            "GET",
            "/api/v1/probe/fingerprint/status",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let fp_data = common::json_data(fp_status).await;
    assert_eq!(fp_data["paused"], true);
    assert_eq!(fp_data["queued_fingerprint"], 1);
    assert_eq!(fp_data["running_fingerprint"], 0);

    let resumed = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/probe/fingerprint/resume",
            Some("management-secret"),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resumed.status(), StatusCode::OK);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let active = state
            .store()
            .active_probe_unit_for_ledger(&ep_ids[0].to_string())
            .unwrap();
        if active.is_none() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "fingerprint did not drain after resume"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_fingerprint_aborts_in_flight_capture() {
    let dir = tempfile::tempdir().unwrap();
    let (fp, entered_rx) = ControllableFingerprint::blocking();
    let state = test_state(dir.path(), fp);
    state.probe.try_start_workers();
    let app = api::router(state.clone());

    let (_lib_id, media_id, ep_ids, _) =
        seed_tv_library_and_episodes(&state.store(), dir.path(), &[1], false);
    let cov = state
        .store()
        .file_coverage_by_id(ep_ids[0])
        .unwrap()
        .unwrap();
    state
        .probe
        .request_probe(&cov, api::probe_manager::policy::ProbeRequestOrigin::Detail)
        .unwrap();

    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(3)))
        .await
        .unwrap()
        .expect("capture did not start");

    let cancel_started = Instant::now();
    let res = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/probe/fingerprint/cancel",
            Some("management-secret"),
            json!({ "media_id": media_id.to_string(), "season": 1, "include_running": true }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let receipt = common::json_data(res).await;
    assert!(receipt["aborted_running"].as_u64().unwrap_or(0) >= 1);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let active = state
            .store()
            .active_probe_unit_for_ledger(&ep_ids[0].to_string())
            .unwrap();
        if active.is_none() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "in-flight fingerprint unit did not terminate after cancel"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(cancel_started.elapsed() < Duration::from_secs(2));
    state.probe.shutdown_workers().await.unwrap();
}

async fn wait_season_markers_done(
    app: &axum::Router,
    lib_id: &str,
    media_id: domain::MediaId,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let res = app
            .clone()
            .oneshot(common::request(
                "GET",
                &format!("/api/v1/libraries/{lib_id}/items/{media_id}/markers?season=1"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let data = common::json_data(res).await;
        if data["active"] == false && data["job"]["status"] == "succeeded" {
            return data;
        }
        assert!(
            Instant::now() < deadline,
            "season detect did not finish: {data}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standalone_season_detect_diagnostics_and_manual_marker_crud() {
    let dir = tempfile::tempdir().unwrap();
    let state = test_state(dir.path(), ControllableFingerprint::fast());
    state.probe.try_start_workers();
    let app = api::router(state.clone());

    let _ = app
        .clone()
        .oneshot(common::request(
            "POST",
            "/api/v1/probe/fingerprint/pause",
            Some("management-secret"),
            json!({}),
        ))
        .await
        .unwrap();

    let (lib_id, media_id, ep_ids, multi_id) =
        seed_tv_library_and_episodes(&state.store(), dir.path(), &[1, 2, 3], true);
    let multi_id = multi_id.unwrap();

    let detect = app
        .clone()
        .oneshot(common::request(
            "POST",
            &format!("/api/v1/libraries/{lib_id}/items/{media_id}/markers/detect"),
            Some("management-secret"),
            json!({ "season": 1, "mode": "recapture" }),
        ))
        .await
        .unwrap();
    assert_eq!(detect.status(), StatusCode::OK);
    let detect_data = common::json_data(detect).await;
    assert_eq!(detect_data["queued"], true);
    assert_eq!(detect_data["skipped_multi_unit"], 1);

    let diag = wait_season_markers_done(&app, &lib_id, media_id).await;

    let episodes = diag["episodes"].as_array().unwrap();
    assert!(
        episodes
            .iter()
            .any(|e| e["file_id"] == multi_id.to_string() && e["multi_unit"] == true)
    );
    for &ep_id in &ep_ids {
        let row = episodes
            .iter()
            .find(|e| e["file_id"] == ep_id.to_string())
            .expect("episode row");
        assert_eq!(row["marker"]["intro_start_ms"], 15_000);
        assert_eq!(row["marker"]["intro_end_ms"], 45_000);
        assert_eq!(row["marker"]["source"], "fingerprint");
    }

    let put = app
        .clone()
        .oneshot(common::request(
            "PUT",
            &format!("/api/v1/libraries/{lib_id}/items/{media_id}/markers"),
            Some("management-secret"),
            json!({
                "file_id": ep_ids[0].to_string(),
                "season": 1,
                "episode": 1,
                "intro_start_ms": 8_000,
                "intro_end_ms": 38_000,
                "outro_start_ms": 1_410_000,
                "outro_end_ms": 1_490_000,
                "locked": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let saved = common::json_data(put).await;
    assert_eq!(saved["locked"], true);
    assert_eq!(saved["source"], "manual");
    assert_eq!(saved["intro_start_ms"], 8_000);

    let del = app
        .clone()
        .oneshot(common::request(
            "DELETE",
            &format!(
                "/api/v1/libraries/{lib_id}/items/{media_id}/markers?file_id={}&season=1&episode=1",
                ep_ids[0]
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::OK);
    assert!(
        state
            .store()
            .get_file_marker(ep_ids[0], UnitKey::new(1, 1))
            .unwrap()
            .is_none()
    );
    state.probe.shutdown_workers().await.unwrap();
}

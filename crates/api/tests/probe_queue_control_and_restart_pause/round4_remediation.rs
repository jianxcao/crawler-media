use std::path::Path;
use std::time::Duration;

use axum::body::to_bytes;
use axum::http::StatusCode;
use domain::{LedgerId, MediaId, MediaKind};
use serde_json::{Value, json};
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

use super::{
    CountingTracks, common, create_lib, in_analysis, insert_test_media, poll_until,
    production_state, seed_scanned_movie,
};

fn seed_tv_episodes(store: &api::Store, root: &Path, show: MediaId, count: u32) -> Vec<LedgerId> {
    (1..=count)
        .map(|ep| {
            let p = root.join(format!("S01E{ep:02}.mkv"));
            std::fs::write(&p, b"tv-bytes").unwrap();
            publish_fixture_file(
                store,
                &FixtureFile {
                    media_id: show,
                    path: p.display().to_string(),
                    season: Some(1),
                    episode: Some(ep),
                    ..Default::default()
                },
            )
            .unwrap()
            .file_id
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_http_resumes_under_writer_contention_keep_paused_until_commit_and_second_succeeds()
 {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("movies-round4-resume");
    std::fs::create_dir_all(&root).unwrap();
    {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        create_lib(&store, MediaKind::Movie, "Movies", &root, true);
        seed_scanned_movie(&store, &root, "ob", 0);
    }
    let state = production_state(dir.path(), CountingTracks::new(false));
    let app = api::router(state.clone());
    assert!(state.probe.background_is_paused());

    let baseline_permits = state.http_work.available_permits();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let writer_store = state.store();
    let writer_holder = std::thread::spawn(move || {
        writer_store
            .library_write(|_| {
                held_tx.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(1_150));
                Ok(())
            })
            .unwrap();
    });
    held_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let app1 = app.clone();
    let first_resume = tokio::spawn(async move {
        app1.oneshot(common::request(
            "POST",
            "/api/v1/probe/resume",
            Some("management-secret"),
            json!({}),
        ))
        .await
        .unwrap()
    });
    poll_until(|| state.http_work.available_permits() < baseline_permits).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        state.probe.background_is_paused(),
        "resume waiting on library_write must not speculatively unpause background probing"
    );

    tokio::time::sleep(Duration::from_millis(420)).await;
    let app2 = app.clone();
    let second_resume = tokio::spawn(async move {
        app2.oneshot(common::request(
            "POST",
            "/api/v1/probe/resume",
            Some("management-secret"),
            json!({}),
        ))
        .await
        .unwrap()
    });

    let first_resp = first_resume.await.unwrap();
    let second_resp = second_resume.await.unwrap();
    writer_holder.join().unwrap();

    assert_eq!(first_resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(second_resp.status(), StatusCode::OK);
    let body = to_bytes(second_resp.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["data"]["paused"], false,
        "second concurrent POST /probe/resume that acquires library_write must unpause cleanly"
    );
    assert!(!state.probe.background_is_paused());
}

fn seed_round4_delete_fixture(dir: &Path) -> (Vec<LedgerId>, String) {
    let root = dir.join("tv-round4-delete");
    let other_root = dir.join("tv-round4-other");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&other_root).unwrap();
    let store = api::Store::open(dir.join("data")).unwrap();
    let lib_id = create_lib(&store, MediaKind::Tv, "TV", &root, true);
    let other_lib_id = create_lib(&store, MediaKind::Tv, "TV-Other", &other_root, false);
    store
        .set_library_intro_settings(&lib_id, true, true)
        .unwrap();
    let show = insert_test_media(&store, MediaKind::Tv, "Delete Retire Show".into());
    let eps = seed_tv_episodes(&store, &root, show, 3);
    let lids: Vec<String> = eps.iter().map(ToString::to_string).collect();
    let specs: Vec<_> = lids
        .iter()
        .map(|lid| store::ProbeJobUnitSpec {
            ledger_id: lid,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: false,
        })
        .collect();
    assert!(
        store
            .create_probe_job(
                "round4-refresh-del",
                "marker_refresh",
                &show.to_string(),
                Some(1),
                &format!("marker-refresh:{show}:1"),
                &specs,
            )
            .unwrap()
    );
    (eps, other_lib_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retiring_last_exempt_unit_via_delete_files_fails_paused_marker_refresh_with_prior_partial_unit()
 {
    use api::probe_manager::policy::ProbeRequestOrigin;

    let dir = tempfile::tempdir().unwrap();
    let (eps, other_lib_id) = seed_round4_delete_fixture(dir.path());
    let state = production_state(dir.path(), CountingTracks::new(false));
    let app = api::router(state.clone());
    assert!(state.probe.background_is_paused());
    assert!(state.probe.try_start_workers());

    // Promote ep1 via playback so it completes capture and enters stage = 'analysis'.
    let row1 = state.store().file_coverage_by_id(eps[0]).unwrap().unwrap();
    state
        .probe
        .request_probe(&row1, ProbeRequestOrigin::Playback)
        .unwrap();
    poll_until(|| in_analysis(&state, eps[0])).await;

    // Transition ep2 directly to terminal `partial` (capture incomplete) and clear ep1's promotion
    // so ep3 will become the sole exempt/promoted unit on the job.
    assert!(
        state
            .store()
            .start_probe_unit(
                "round4-refresh-del",
                &eps[1].to_string(),
                store::ProbeDispatchStage::Metadata,
            )
            .unwrap()
    );
    state
        .store()
        .finish_probe_unit_with_status(
            "round4-refresh-del",
            &eps[1].to_string(),
            "partial",
            Some("timeout"),
            Some("tail unavailable"),
        )
        .unwrap();

    // Stop workers before promoting ep3 so `(job_id, ep3)` stays queued in `paused_exempt_units`,
    // then delete ep3 (and ep1's promotion is reset via pause_background + re-promote ep3).
    state.probe.shutdown_workers().await.unwrap();
    state.probe.pause_background();
    let row3 = state.store().file_coverage_by_id(eps[2]).unwrap().unwrap();
    state
        .probe
        .request_probe(&row3, ProbeRequestOrigin::Playback)
        .unwrap();
    assert!(state.probe.has_paused_exempt_work());

    state
        .store()
        .delete_ledger_path(row3.file.path.to_str().unwrap())
        .unwrap();
    super::admin_json(
        &app,
        "POST",
        "/api/v1/probe/cancel",
        json!({ "library_id": other_lib_id }),
    )
    .await;

    let job = state
        .store()
        .get_probe_job("round4-refresh-del")
        .unwrap()
        .unwrap();
    assert_eq!(
        job.status, "failed",
        "retiring last non-analysis unit when a prior unit was partial must fail marker_refresh"
    );
    assert!(
        state
            .store()
            .active_probe_unit_for_ledger(&eps[0].to_string())
            .unwrap()
            .is_none()
    );
}

fn seed_two_lib_partial_refresh(dir: &Path, job_id: &str) -> (String, LedgerId, LedgerId) {
    let root_a = dir.join(format!("tv-a-{job_id}"));
    let root_b = dir.join(format!("tv-b-{job_id}"));
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let store = api::Store::open(dir.join("data")).unwrap();
    let lib_a = create_lib(&store, MediaKind::Tv, "TV-A", &root_a, true);
    let lib_b = create_lib(&store, MediaKind::Tv, "TV-B", &root_b, false);
    store
        .set_library_intro_settings(&lib_a, true, true)
        .unwrap();
    store
        .set_library_intro_settings(&lib_b, true, true)
        .unwrap();
    let show = insert_test_media(&store, MediaKind::Tv, format!("Show {job_id}"));
    let ep1 = seed_tv_episodes(&store, &root_a, show, 1)[0];
    let p2 = root_b.join("S01E02.mkv");
    std::fs::write(&p2, b"tv-bytes").unwrap();
    let ep2 = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: show,
            path: p2.display().to_string(),
            season: Some(1),
            episode: Some(2),
            ..Default::default()
        },
    )
    .unwrap()
    .file_id;
    let (l1, l2) = (ep1.to_string(), ep2.to_string());
    let specs = [&l1, &l2].map(|lid| store::ProbeJobUnitSpec {
        ledger_id: lid,
        kind: "tv",
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: true,
        reuse_media_info_cache: false,
    });
    assert!(
        store
            .create_probe_job(
                job_id,
                "marker_refresh",
                &show.to_string(),
                Some(1),
                &format!("marker-refresh:{show}:1"),
                &specs,
            )
            .unwrap()
    );
    assert!(
        store
            .start_probe_unit(job_id, &l1, store::ProbeDispatchStage::Metadata)
            .unwrap()
    );
    store
        .finish_probe_unit_with_status(job_id, &l1, "partial", Some("timeout"), Some("tail err"))
        .unwrap();
    (lib_b, ep1, ep2)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_last_unit_of_partial_marker_refresh_fails_job_in_recovery_and_worker_cancel() {
    use api::probe_manager::policy::ProbeRequestOrigin;

    // 1. Startup recovery cancelling unit in removed library when prior unit was partial -> failed.
    let dir_rec = tempfile::tempdir().unwrap();
    let (lib_b, _ep1, _ep2) = seed_two_lib_partial_refresh(dir_rec.path(), "r5-rec-partial");
    {
        let store = api::Store::open(dir_rec.path().join("data")).unwrap();
        store.begin_library_removal(&lib_b).unwrap();
    }
    let state_rec = production_state(dir_rec.path(), CountingTracks::new(false));
    let rec_job = state_rec
        .store()
        .get_probe_job("r5-rec-partial")
        .unwrap()
        .unwrap();
    assert_eq!(
        rec_job.status, "failed",
        "startup recovery cancelling last unit of partial marker_refresh must fail job"
    );

    // 2. Worker cancelling deleted file on disk when prior unit was partial -> failed (not cancelled).
    let dir_wrk = tempfile::tempdir().unwrap();
    let (_lib_b2, _w_ep1, w_ep2) = seed_two_lib_partial_refresh(dir_wrk.path(), "r5-wrk-partial");
    let state_wrk = production_state(dir_wrk.path(), CountingTracks::new(false));
    let row2 = state_wrk
        .store()
        .file_coverage_by_id(w_ep2)
        .unwrap()
        .unwrap();
    std::fs::remove_file(&row2.file.path).unwrap();
    assert!(state_wrk.probe.try_start_workers());
    state_wrk
        .probe
        .request_probe(&row2, ProbeRequestOrigin::Playback)
        .unwrap();
    poll_until(|| {
        !state_wrk
            .store()
            .get_probe_job("r5-wrk-partial")
            .unwrap()
            .unwrap()
            .is_active()
    })
    .await;
    let wrk_job = state_wrk
        .store()
        .get_probe_job("r5-wrk-partial")
        .unwrap()
        .unwrap();
    assert_eq!(
        wrk_job.status, "failed",
        "worker cancelling missing source when prior unit was partial must fail marker_refresh"
    );
    state_wrk.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn store_deleting_unpromoted_sibling_completes_paused_unit_promoted_marker_refresh() {
    use api::probe_manager::policy::ProbeRequestOrigin;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tv-r5-promoted-sibling");
    std::fs::create_dir_all(&root).unwrap();
    let eps = {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        let lib_id = create_lib(&store, MediaKind::Tv, "TV", &root, true);
        store
            .set_library_intro_settings(&lib_id, true, true)
            .unwrap();
        let show = insert_test_media(&store, MediaKind::Tv, "Promoted Sibling Show".into());
        let eps = seed_tv_episodes(&store, &root, show, 2);
        let lids: Vec<String> = eps.iter().map(ToString::to_string).collect();
        let specs: Vec<_> = lids
            .iter()
            .map(|lid| store::ProbeJobUnitSpec {
                ledger_id: lid,
                kind: "tv",
                force_fingerprint: true,
                reuse_fingerprint_cache: false,
                overwrite_markers: true,
                reuse_media_info_cache: false,
            })
            .collect();
        assert!(
            store
                .create_probe_job(
                    "r5-promoted-refresh",
                    "marker_refresh",
                    &show.to_string(),
                    Some(1),
                    &format!("marker-refresh:{show}:1"),
                    &specs,
                )
                .unwrap()
        );
        eps
    };

    let state = production_state(dir.path(), CountingTracks::new(false));
    assert!(state.probe.background_is_paused());
    assert!(state.probe.try_start_workers());

    let row1 = state.store().file_coverage_by_id(eps[0]).unwrap().unwrap();
    state
        .probe
        .request_probe(&row1, ProbeRequestOrigin::Playback)
        .unwrap();
    poll_until(|| in_analysis(&state, eps[0])).await;
    assert!(!state.probe.has_paused_exempt_work());

    let row2 = state.store().file_coverage_by_id(eps[1]).unwrap().unwrap();
    state
        .store()
        .delete_ledger_path(row2.file.path.to_str().unwrap())
        .unwrap();

    poll_until(|| {
        state
            .store()
            .get_probe_job("r5-promoted-refresh")
            .unwrap()
            .unwrap()
            .status
            == "succeeded"
    })
    .await;
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chapter_refresh_endpoint_supersedes_dormant_paused_unit_and_queues_marker_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tv-r5-chapter-refresh");
    std::fs::create_dir_all(&root).unwrap();
    let (tv_lib_id, show, ep1) = {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        store
            .save_raw_scrape_settings(
                &json!({ "fingerprint_sampling_mode": "adaptive" }).to_string(),
            )
            .unwrap();
        let lib_id = create_lib(&store, MediaKind::Tv, "TV", &root, true);
        store
            .set_library_intro_settings(&lib_id, true, true)
            .unwrap();
        let show = insert_test_media(&store, MediaKind::Tv, "Chapter Refresh Show".into());
        let ep1 = seed_tv_episodes(&store, &root, show, 1)[0];
        let lid = ep1.to_string();
        assert!(
            store
                .create_probe_job(
                    "dormant-media-probe",
                    "media_probe",
                    &show.to_string(),
                    Some(1),
                    &format!("ledger:{lid}"),
                    &[store::ProbeJobUnitSpec {
                        ledger_id: &lid,
                        kind: "tv",
                        force_fingerprint: false,
                        reuse_fingerprint_cache: true,
                        overwrite_markers: false,
                        reuse_media_info_cache: true,
                    }],
                )
                .unwrap()
        );
        (lib_id, show, ep1)
    };

    let state = production_state(dir.path(), CountingTracks::new(false));
    assert!(state.probe.background_is_paused());
    let app = api::router(state.clone());

    let path = format!(
        "/api/v1/libraries/{tv_lib_id}/items/{show}/chapters/refresh?file_id={ep1}&season=1&episode=1"
    );
    let data = super::admin_json(
        &app,
        "POST",
        &path,
        json!({ "capture_policy": "recapture" }),
    )
    .await;
    assert_eq!(data["fingerprint_refresh_queued"], true);
    assert_eq!(data["fingerprint_refresh_already_running"], false);
    assert_eq!(data["fingerprint_refresh_job"]["kind"], "marker_refresh");
    assert_eq!(
        state
            .store()
            .get_probe_job("dormant-media-probe")
            .unwrap()
            .unwrap()
            .status,
        "cancelled"
    );
}

struct FailingCaptureTracks(CountingTracks);

impl marker::FingerprintEngine for FailingCaptureTracks {
    fn extract_at(
        &self,
        _p: &Path,
        _s: u32,
        _d: u32,
        _c: &marker::WorkControl,
    ) -> Result<Vec<u32>, String> {
        Err("simulated full window failure".into())
    }

    fn find_common_segment(
        &self,
        a: &[u32],
        b: &[u32],
        m: f32,
        t: f32,
    ) -> Option<marker::CommonSegment> {
        self.0.find_common_segment(a, b, m, t)
    }
}

impl marker::FingerprintCaptureEngine for FailingCaptureTracks {
    fn capture_window(
        &self,
        req: &marker::CaptureRequest,
    ) -> Result<marker::CapturedFingerprint, marker::CaptureFailure> {
        if req.window.start_ms == 0 {
            return self.0.capture_window(req);
        }
        Err(marker::CaptureFailure {
            kind: marker::CaptureFailureKind::Timeout,
            message: "simulated outro timeout".into(),
            metrics: marker::CaptureMetrics::default(),
        })
    }
}

impl api::probe_manager::TracksProbe for FailingCaptureTracks {
    fn probe(
        &self,
        path: &Path,
        control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        self.0.probe(path, control)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paused_single_episode_marker_refresh_with_partial_capture_clears_exemption_and_finalizes_timings()
 {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tv-r6-partial-timings");
    std::fs::create_dir_all(&root).unwrap();
    let (tv_lib_id, show, ep1) = {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        store
            .save_raw_scrape_settings(
                &json!({ "fingerprint_sampling_mode": "adaptive" }).to_string(),
            )
            .unwrap();
        let lib_id = create_lib(&store, MediaKind::Tv, "TV", &root, true);
        store
            .set_library_intro_settings(&lib_id, true, true)
            .unwrap();
        let show = insert_test_media(&store, MediaKind::Tv, "Partial Timings Show".into());
        let ep1 = seed_tv_episodes(&store, &root, show, 2)[0];
        (lib_id, show, ep1)
    };

    let engine = std::sync::Arc::new(FailingCaptureTracks(CountingTracks {
        calls: std::sync::atomic::AtomicUsize::new(0),
        fail: std::sync::atomic::AtomicBool::new(false),
        block_first: parking_lot::Mutex::new(None),
    }));
    let base = production_state(dir.path(), CountingTracks::new(false));
    let state = base
        .with_probe_engines(engine.clone(), engine.clone(), engine)
        .unwrap();
    assert!(state.probe.background_is_paused());
    assert!(state.probe.try_start_workers());
    let app = api::router(state.clone());

    let path = format!(
        "/api/v1/libraries/{tv_lib_id}/items/{show}/chapters/refresh?file_id={ep1}&season=1&episode=1"
    );
    let data = super::admin_json(
        &app,
        "POST",
        &path,
        json!({ "capture_policy": "recapture" }),
    )
    .await;
    let job_id = data["fingerprint_refresh_job"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    poll_until(|| {
        state
            .store()
            .get_probe_job(&job_id)
            .unwrap()
            .unwrap()
            .status
            == "failed"
    })
    .await;
    let final_job = state.store().get_probe_job(&job_id).unwrap().unwrap();
    assert_eq!(
        (final_job.failed, final_job.partial),
        (0, 2),
        "{final_job:?}"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    state.probe.shutdown_workers().await.unwrap();

    assert!(
        !state.probe.has_paused_exempt_work(),
        "failed all-done partial marker_refresh must clear paused_exempt_jobs"
    );
    let snapshot = state
        .probe
        .timing_snapshot(&job_id)
        .expect("timing snapshot");
    assert!(
        snapshot.persisted_attempts_complete,
        "failed all-done partial marker_refresh must finalize terminal job timings"
    );
}

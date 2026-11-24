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

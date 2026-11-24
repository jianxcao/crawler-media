use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::http::StatusCode;
use domain::{LedgerId, LibraryId, MediaId, MediaKind};
use serde_json::json;
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;

use super::remediation_cases::{AdaptiveFallbackTracks, adaptive_production_state};
use super::{
    CountingTracks, admin_json, common, create_lib, create_queued_job, has_meta, in_analysis,
    insert_test_media, poll_until, production_state, seed_scanned_movie,
};

fn create_marker_refresh_job(
    store: &api::Store,
    job_id: &str,
    show: MediaId,
    season: Option<u32>,
    scope: &str,
    episodes: &[LedgerId],
) {
    let lids: Vec<String> = episodes.iter().map(ToString::to_string).collect();
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
                job_id,
                "marker_refresh",
                &show.to_string(),
                season,
                scope,
                &specs
            )
            .unwrap()
    );
}

fn seed_season_episodes(
    store: &api::Store,
    root: &Path,
    show: MediaId,
    season: u32,
    count: u32,
) -> Vec<LedgerId> {
    (1..=count)
        .map(|ep| {
            let p = root.join(format!("S{season:02}E{ep:02}.mkv"));
            std::fs::write(&p, b"tv-bytes").unwrap();
            let spec = FixtureFile {
                media_id: show,
                path: p.display().to_string(),
                season: Some(season),
                episode: Some(ep),
                ..Default::default()
            };
            publish_fixture_file(store, &spec).unwrap().file_id
        })
        .collect()
}

fn hold_all_library_readers(
    store: &Arc<api::Store>,
) -> (
    Vec<std::thread::JoinHandle<()>>,
    std::sync::mpsc::Sender<()>,
) {
    let barrier = Arc::new(std::sync::Barrier::new(9));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let shared_rx = Arc::new(parking_lot::Mutex::new(release_rx));
    let mut handles = Vec::with_capacity(8);
    for _ in 0..8 {
        let pool_store = Arc::clone(store);
        let b = Arc::clone(&barrier);
        let rx = Arc::clone(&shared_rx);
        handles.push(std::thread::spawn(move || {
            pool_store
                .with_read_snapshot(&[store::Db::Library], Duration::from_secs(5), || {
                    b.wait();
                    let _ = rx.lock().recv_timeout(Duration::from_secs(5));
                    Ok::<_, store::StoreError>(())
                })
                .unwrap()
                .unwrap();
        }));
    }
    barrier.wait();
    (handles, release_tx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_endpoint_succeeds_even_when_write_gate_closes_after_sql_cancel_commits() {
    use api::probe_manager::policy::ProbeRequestOrigin;

    let dir = tempfile::tempdir().unwrap();
    let root_a = dir.path().join("tv-lib-a");
    let root_b = dir.path().join("tv-lib-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let (lib_b_id, ep_a, ep_b) = {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        let lib_a_id = create_lib(&store, MediaKind::Tv, "TV-A", &root_a, true);
        let lib_b_id = create_lib(&store, MediaKind::Tv, "TV-B", &root_b, true);
        store
            .set_library_intro_settings(&lib_a_id, true, true)
            .unwrap();
        store
            .set_library_intro_settings(&lib_b_id, true, true)
            .unwrap();
        let show = insert_test_media(&store, MediaKind::Tv, "CrossLib Show".into());
        let ep_a = seed_season_episodes(&store, &root_a, show, 1, 1)[0];
        let p_b = root_b.join("S01E02.mkv");
        std::fs::write(&p_b, b"tv-bytes").unwrap();
        let ep_b = publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: show,
                path: p_b.display().to_string(),
                season: Some(1),
                episode: Some(2),
                ..Default::default()
            },
        )
        .unwrap()
        .file_id;
        create_marker_refresh_job(
            &store,
            "cross-lib-refresh",
            show,
            Some(1),
            &format!("marker-refresh:{show}:1"),
            &[ep_a, ep_b],
        );
        (lib_b_id, ep_a, ep_b)
    };

    let state = production_state(dir.path(), CountingTracks::new(false));
    assert!(state.probe.try_start_workers());
    let app = api::router(state.clone());

    let row_a = state.store().file_coverage_by_id(ep_a).unwrap().unwrap();
    state
        .probe
        .request_probe(&row_a, ProbeRequestOrigin::Playback)
        .unwrap();
    poll_until(|| in_analysis(&state, ep_a)).await;

    let store_arc = state.store();
    let (readers, release_tx) = hold_all_library_readers(&store_arc);
    let cancel_app = app.clone();
    let cancel_task = tokio::spawn(async move {
        cancel_app
            .oneshot(common::request(
                "POST",
                "/api/v1/probe/cancel",
                Some("management-secret"),
                json!({ "library_id": lib_b_id }),
            ))
            .await
            .unwrap()
    });

    let db_path = dir.path().join("data/library.db");
    let raw_ro =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let ep_b_str = ep_b.to_string();
    poll_until(|| {
        raw_ro
            .query_row(
                "SELECT status FROM probe_job_units WHERE job_id = 'cross-lib-refresh' AND file_id = ?1",
                [&ep_b_str],
                |row| row.get::<_, String>(0),
            )
            .is_ok_and(|status| status == "cancelled")
    })
    .await;

    let gate = state.write_gate();
    let gate_guard = gate.close();
    drop(release_tx);
    for h in readers {
        h.join().unwrap();
    }

    let response = cancel_task.await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "already-committed cancel must return 200 OK even if write_gate closed before purge_cancelled_seen"
    );
    let data = common::json_data(response).await;
    assert_eq!(data["cancelled_units"], 1);
    drop(gate_guard);
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_resume_preserves_preexisting_paused_exemptions_and_runs_exempt_work() {
    use api::probe_manager::policy::ProbeRequestOrigin;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("movies-exempt-survive");
    std::fs::create_dir_all(&root).unwrap();
    let (f_exempt, f_bg) = {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        create_lib(&store, MediaKind::Movie, "Movies", &root, true);
        let (_, f_exempt) = seed_scanned_movie(&store, &root, "exempt", 0);
        let (m_bg, f_bg) = seed_scanned_movie(&store, &root, "bg", 1);
        create_queued_job(&store, m_bg, f_bg, false);
        (f_exempt, f_bg)
    };
    let state = production_state(dir.path(), CountingTracks::new(false));
    let app = api::router(state.clone());

    let exempt_row = state
        .store()
        .file_coverage_by_id(f_exempt)
        .unwrap()
        .unwrap();
    state
        .probe
        .request_probe(&exempt_row, ProbeRequestOrigin::Playback)
        .unwrap();
    assert!(state.probe.has_paused_exempt_work());

    let baseline_permits = state.http_work.available_permits();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let writer_store = state.store();
    let writer_holder = std::thread::spawn(move || {
        writer_store
            .library_write(|_| {
                held_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
            .unwrap();
    });
    held_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let resume_app = app.clone();
    let resume_task = tokio::spawn(async move {
        resume_app
            .oneshot(common::request(
                "POST",
                "/api/v1/probe/resume",
                Some("management-secret"),
                json!({}),
            ))
            .await
            .unwrap()
    });
    poll_until(|| state.http_work.available_permits() < baseline_permits).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let gate = state.write_gate();
    let gate_guard = gate.close();
    release_tx.send(()).unwrap();
    writer_holder.join().unwrap();

    let refused = resume_task.await.unwrap();
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(gate_guard);

    assert!(state.probe.background_is_paused());
    assert!(
        state.probe.has_paused_exempt_work(),
        "rollback of uncommitted resume must preserve pre-existing paused exemptions"
    );
    assert!(
        state.probe.holds_dispatch_id(&f_exempt.to_string()),
        "rollback of uncommitted resume must keep or refill exempt dispatch window"
    );

    assert!(state.probe.try_start_workers());
    poll_until(|| has_meta(&state, f_exempt)).await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(!has_meta(&state, f_bg));
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_resume_commit_is_not_reverted_by_older_resume_rollback_and_tolerates_obligation_refusal()
 {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("movies-concurrent-resume");
    std::fs::create_dir_all(&root).unwrap();
    {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        create_lib(&store, MediaKind::Movie, "Movies", &root, true);
        seed_scanned_movie(&store, &root, "ob", 0);
        seed_scanned_movie(&store, &root, "ob", 1);
    }
    let state = production_state(dir.path(), CountingTracks::new(false));
    let app = api::router(state.clone());

    // 1. Older resume timing out on Library writer must NOT revert a concurrent committed resume.
    let baseline_permits = state.http_work.available_permits();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let writer_store = state.store();
    let writer_holder = std::thread::spawn(move || {
        writer_store
            .library_write(|_| {
                held_tx.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(1_100));
                Ok(())
            })
            .unwrap();
    });
    held_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let resume_app = app.clone();
    let slow_resume = tokio::spawn(async move {
        resume_app
            .oneshot(common::request(
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
    state.probe.resume_background();
    assert!(!state.probe.background_is_paused());

    let timed_out = slow_resume.await.unwrap();
    writer_holder.join().unwrap();
    assert_eq!(timed_out.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        !state.probe.background_is_paused(),
        "older resume rollback must not re-pause after a concurrent resume already committed"
    );

    // 2. Transient WriteBusy + write-gate refusal during post-commit obligation dispatch must keep 200 OK.
    admin_json(&app, "POST", "/api/v1/probe/pause", json!({})).await;
    assert!(state.probe.background_is_paused());

    let store_arc = state.store();
    let (readers, release_tx) = hold_all_library_readers(&store_arc);
    let resume_app2 = app.clone();
    let resume_task2 = tokio::spawn(async move {
        resume_app2
            .oneshot(common::request(
                "POST",
                "/api/v1/probe/resume",
                Some("management-secret"),
                json!({}),
            ))
            .await
            .unwrap()
    });
    poll_until(|| !state.probe.background_is_paused()).await;
    let (held_tx2, held_rx2) = std::sync::mpsc::channel();
    let writer_store2 = state.store();
    let busy_writer = std::thread::spawn(move || {
        writer_store2
            .library_write(|_| {
                held_tx2.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(1_100));
                Ok(())
            })
            .unwrap();
    });
    held_rx2.recv_timeout(Duration::from_secs(2)).unwrap();
    let gate = state.write_gate();
    let gate_guard = gate.close();
    drop(release_tx);
    for h in readers {
        h.join().unwrap();
    }

    let resp2 = resume_task2.await.unwrap();
    busy_writer.join().unwrap();
    drop(gate_guard);
    assert_eq!(
        resp2.status(),
        StatusCode::OK,
        "post-commit obligation dispatch WriteBusy/gate closure must not fail resume with 503"
    );
    assert!(!state.probe.background_is_paused());
    assert!(
        !state.store().scan_probe_obligations(10).unwrap().is_empty(),
        "unacked obligations on WriteBusy must remain queued for background retry"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiseason_adaptive_marker_refresh_completes_second_window_across_seasons_while_paused() {
    use api::probe_manager::policy::ProbeRequestOrigin;

    let dir = tempfile::tempdir().unwrap();
    let tv_root = dir.path().join("tv-multiseason-adaptive");
    std::fs::create_dir_all(&tv_root).unwrap();
    let (s1_eps, s2_eps) = {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        store
            .save_raw_scrape_settings(
                &json!({ "fingerprint_sampling_mode": "adaptive" }).to_string(),
            )
            .unwrap();
        let lib_id = create_lib(&store, MediaKind::Tv, "TV", &tv_root, true);
        store
            .set_library_intro_settings(&lib_id, true, true)
            .unwrap();
        let show = insert_test_media(&store, MediaKind::Tv, "MultiSeason Adaptive".into());
        let s1 = seed_season_episodes(&store, &tv_root, show, 1, 3);
        let s2 = seed_season_episodes(&store, &tv_root, show, 2, 3);
        let all_eps: Vec<_> = s1.iter().chain(s2.iter()).copied().collect();
        create_marker_refresh_job(
            &store,
            "multiseason-adaptive-all",
            show,
            None,
            &format!("marker-refresh:{show}:all"),
            &all_eps,
        );
        (s1, s2)
    };

    let tracks = AdaptiveFallbackTracks::new();
    let state = adaptive_production_state(dir.path(), tracks.clone());
    let clock = Arc::new(api::probe_manager::clock::FakeClock::new(1_000));
    state.probe.set_clock(clock.clone());
    assert!(state.probe.try_start_workers());

    for &ep in &s1_eps {
        let row = state.store().file_coverage_by_id(ep).unwrap().unwrap();
        state
            .probe
            .request_probe(&row, ProbeRequestOrigin::Playback)
            .unwrap();
    }
    poll_until(|| s1_eps.iter().all(|&ep| in_analysis(&state, ep))).await;
    state.probe.pause_background();

    for &ep in &s2_eps {
        let row = state.store().file_coverage_by_id(ep).unwrap().unwrap();
        state
            .probe
            .request_probe(&row, ProbeRequestOrigin::Playback)
            .unwrap();
    }

    let mut now = 1_000;
    poll_until(|| {
        now += 2_500;
        clock.set(now);
        state.probe.wake_dispatch();
        state
            .store()
            .get_probe_job("multiseason-adaptive-all")
            .unwrap()
            .unwrap()
            .status
            == "succeeded"
    })
    .await;
    assert!(tracks.captures.load(Ordering::SeqCst) >= 18);
    assert!(!state.probe.has_paused_exempt_work());
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paused_manual_supersede_protects_promoted_analysis_and_reconciles_retired_marker_refresh()
{
    use api::probe_manager::policy::ProbeRequestOrigin;

    let dir = tempfile::tempdir().unwrap();
    let tv_root = dir.path().join("tv-supersede");
    std::fs::create_dir_all(&tv_root).unwrap();
    let (lib_id, show, ep_ids) = {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        let lib_id = create_lib(&store, MediaKind::Tv, "TV", &tv_root, true);
        store
            .set_library_intro_settings(&lib_id, true, true)
            .unwrap();
        let show = insert_test_media(&store, MediaKind::Tv, "Supersede Show".into());
        let ids = seed_season_episodes(&store, &tv_root, show, 1, 2);
        create_marker_refresh_job(
            &store,
            "refresh-supersede",
            show,
            Some(1),
            &format!("marker-refresh:{show}:1"),
            &ids,
        );
        (lib_id, show, ids)
    };

    let state = production_state(dir.path(), CountingTracks::new(false));
    let clock = Arc::new(api::probe_manager::clock::FakeClock::new(1_000));
    state.probe.set_clock(clock.clone());
    assert!(state.probe.try_start_workers());
    let app = api::router(state.clone());

    let row1 = state
        .store()
        .file_coverage_by_id(ep_ids[0])
        .unwrap()
        .unwrap();
    state
        .probe
        .request_probe(&row1, ProbeRequestOrigin::Playback)
        .unwrap();
    poll_until(|| in_analysis(&state, ep_ids[0])).await;
    assert!(!state.probe.has_paused_exempt_work());

    // 1. Already-promoted ep1 in stage='analysis' must NOT be superseded as dormant.
    assert!(
        !state.probe.enqueue_force(api::probe_manager::ProbeUnit {
            row: row1.clone(),
            kind: MediaKind::Tv,
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: false,
            reuse_media_info_cache: false,
            marker_refresh_id: None,
            job_id: None,
        }),
        "already-promoted marker_refresh unit in analysis must not be superseded as dormant"
    );
    let item_probe = admin_json(
        &app,
        "POST",
        &format!("/api/v1/libraries/{lib_id}/items/{show}/probe"),
        json!({}),
    )
    .await;
    assert_eq!(item_probe["already_running"], true);

    // 2. Superseding unpromoted dormant sibling ep2 must reconcile refresh-supersede so ep1 finishes!
    let row2 = state
        .store()
        .file_coverage_by_id(ep_ids[1])
        .unwrap()
        .unwrap();
    assert!(state.probe.enqueue_force(api::probe_manager::ProbeUnit {
        row: row2,
        kind: MediaKind::Tv,
        force_fingerprint: true,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: false,
        marker_refresh_id: None,
        job_id: None,
    }));

    let mut now = 1_000;
    poll_until(|| {
        now += 2_500;
        clock.set(now);
        state.probe.wake_dispatch();
        state
            .store()
            .get_probe_job("refresh-supersede")
            .unwrap()
            .unwrap()
            .status
            == "succeeded"
            && state
                .store()
                .active_probe_unit_for_ledger(&ep_ids[1].to_string())
                .unwrap()
                .is_none()
    })
    .await;
    assert!(!state.probe.has_paused_exempt_work());
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleted_file_dispatch_clears_exemptions_and_reconciles_surviving_promoted_marker_refresh()
{
    use api::probe_manager::policy::{ProbeRequestOrigin, ProbeRequestResult};

    let dir = tempfile::tempdir().unwrap();
    let movie_root = dir.path().join("movies-del");
    let tv_root = dir.path().join("tv-del");
    std::fs::create_dir_all(&movie_root).unwrap();
    std::fs::create_dir_all(&tv_root).unwrap();
    let (movie_lib_id, tv_lib_id, m_del, m_block, ep_ids) = {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        let movie_lib_id = create_lib(&store, MediaKind::Movie, "Movies", &movie_root, false);
        let (_, m_del) = seed_scanned_movie(&store, &movie_root, "del", 0);
        let (_, m_block) = seed_scanned_movie(&store, &movie_root, "block", 1);
        let tv_lib_id = create_lib(&store, MediaKind::Tv, "TV", &tv_root, true);
        store
            .set_library_intro_settings(&tv_lib_id, true, true)
            .unwrap();
        let show = insert_test_media(&store, MediaKind::Tv, "Deleted Dispatch TV".into());
        let ids = seed_season_episodes(&store, &tv_root, show, 1, 2);
        create_marker_refresh_job(
            &store,
            "refresh-del-dispatch",
            show,
            Some(1),
            &format!("marker-refresh:{show}:1"),
            &ids,
        );
        (movie_lib_id, tv_lib_id, m_del, m_block, ids)
    };

    let tracks = CountingTracks::new(false);
    let state = production_state(dir.path(), tracks.clone());
    let clock = Arc::new(api::probe_manager::clock::FakeClock::new(1_000));
    state.probe.set_clock(clock.clone());

    // 1. Playback-exempt job whose file is deleted before metadata worker loads it clears exemptions.
    let del_row = state.store().file_coverage_by_id(m_del).unwrap().unwrap();
    let ProbeRequestResult::Queued { job_id: del_job_id } = state
        .probe
        .request_probe(&del_row, ProbeRequestOrigin::Playback)
        .unwrap()
    else {
        panic!("expected Queued for m_del");
    };
    assert!(state.probe.has_paused_exempt_work());
    let movie_lib: LibraryId = movie_lib_id.parse().unwrap();
    assert_eq!(state.store().delete_files(movie_lib, &[m_del]).unwrap(), 1);

    assert!(state.probe.try_start_workers());
    poll_until(|| !state.probe.has_paused_exempt_work()).await;
    assert!(
        state
            .store()
            .get_probe_job(&del_job_id)
            .unwrap()
            .is_none_or(|job| job.status == "cancelled")
    );

    // 2. Promoted marker_refresh ep1 in analysis + promoted ep2 deleted while buffered in metadata_tx.
    let row_ep1 = state
        .store()
        .file_coverage_by_id(ep_ids[0])
        .unwrap()
        .unwrap();
    let row_ep2 = state
        .store()
        .file_coverage_by_id(ep_ids[1])
        .unwrap()
        .unwrap();
    state
        .probe
        .request_probe(&row_ep1, ProbeRequestOrigin::Playback)
        .unwrap();
    poll_until(|| in_analysis(&state, ep_ids[0])).await;

    let (entered_rx, release_tx) = tracks.arm_block_next();
    let block_row = state.store().file_coverage_by_id(m_block).unwrap().unwrap();
    state
        .probe
        .request_probe(&block_row, ProbeRequestOrigin::Playback)
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();

    let tv_lib: LibraryId = tv_lib_id.parse().unwrap();
    state
        .probe
        .request_probe(&row_ep2, ProbeRequestOrigin::Playback)
        .unwrap();
    assert_eq!(state.store().delete_files(tv_lib, &[ep_ids[1]]).unwrap(), 1);
    release_tx.send(()).unwrap();
    let _ = movie_lib_id;

    let mut now = 1_000;
    poll_until(|| {
        now += 2_500;
        clock.set(now);
        state.probe.wake_dispatch();
        state
            .store()
            .get_probe_job("refresh-del-dispatch")
            .unwrap()
            .unwrap()
            .status
            == "succeeded"
    })
    .await;
    assert!(!state.probe.has_paused_exempt_work());
    state.probe.shutdown_workers().await.unwrap();
}

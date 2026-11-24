use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::http::StatusCode;
use domain::MediaKind;
use serde_json::json;
use tower::ServiceExt;

use super::{
    CountingTracks, admin_json, common, create_lib, has_meta, in_analysis, poll_until,
    production_state, production_state_with_writable, seed_scanned_movie, seed_tv_episodes,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promoting_one_episode_of_paused_marker_refresh_keeps_siblings_queued_and_clears_exemption()
{
    use api::probe_manager::policy::{ProbeRequestOrigin, ProbeRequestResult};
    let dir = tempfile::tempdir().unwrap();
    let tv_root = dir.path().join("shows");
    std::fs::create_dir_all(&tv_root).unwrap();
    let ep_ids = {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        let (show, ids) = seed_tv_episodes(&store, &tv_root, 3);
        let strings: Vec<String> = ids.iter().map(ToString::to_string).collect();
        let specs: Vec<_> = strings
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
        let scope = format!("marker-refresh:{show}:1");
        assert!(
            store
                .create_probe_job(
                    "season-refresh-1",
                    "marker_refresh",
                    &show.to_string(),
                    Some(1),
                    &scope,
                    &specs
                )
                .unwrap()
        );
        ids
    };
    let tracks = CountingTracks::new(false);
    let state = production_state(dir.path(), tracks.clone());
    let clock = Arc::new(api::probe_manager::clock::FakeClock::new(1_000));
    state.probe.set_clock(clock.clone());
    assert!(state.probe.try_start_workers());
    let ep1_row = state
        .store()
        .file_coverage_by_id(ep_ids[0])
        .unwrap()
        .unwrap();
    let promoted = state
        .probe
        .request_probe(&ep1_row, ProbeRequestOrigin::Playback)
        .unwrap();
    assert_eq!(
        promoted,
        ProbeRequestResult::Queued {
            job_id: "season-refresh-1".into()
        }
    );
    poll_until(|| in_analysis(&state, ep_ids[0])).await;
    assert!(
        !state.probe.has_paused_exempt_work(),
        "promoted marker_refresh unit waiting on unpromoted siblings must clear unit exemption on handoff"
    );
    assert!(state.probe.background_is_paused());
    assert!(has_meta(&state, ep_ids[0]));
    for &sibling in &ep_ids[1..] {
        assert!(!has_meta(&state, sibling));
    }
    let units = state.store().probe_job_units("season-refresh-1").unwrap();
    assert_eq!(
        units
            .iter()
            .filter(|u| u.status == "queued" && u.stage.as_str() == "metadata")
            .count(),
        2
    );
    assert_eq!(tracks.calls(), 1);

    // Re-promoting an episode already in analysis also clears exemption via compute::refresh.
    state
        .probe
        .request_probe(&ep1_row, ProbeRequestOrigin::Playback)
        .unwrap();
    let mut now = 1_000;
    poll_until(|| {
        now += 2_500;
        clock.set(now);
        state.probe.wake_dispatch();
        !state.probe.has_paused_exempt_work()
    })
    .await;
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paused_analysis_runs_for_exempt_episode_sorting_after_16_non_exempt_rows() {
    use api::probe_manager::policy::ProbeRequestOrigin;
    let dir = tempfile::tempdir().unwrap();
    let tv_root = dir.path().join("tv-large-season");
    std::fs::create_dir_all(&tv_root).unwrap();
    let (show, ep_ids) = {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        seed_tv_episodes(&store, &tv_root, 18)
    };
    let tracks = CountingTracks::new(false);
    let state = production_state(dir.path(), tracks);
    let clock = Arc::new(api::probe_manager::clock::FakeClock::new(1_000));
    state.probe.set_clock(clock.clone());
    assert!(state.probe.try_start_workers());
    let app = api::router(state.clone());

    for &ep in &ep_ids {
        let row = state.store().file_coverage_by_id(ep).unwrap().unwrap();
        state
            .probe
            .request_probe(&row, ProbeRequestOrigin::Playback)
            .unwrap();
    }
    poll_until(|| ep_ids.iter().all(|&ep| in_analysis(&state, ep))).await;
    admin_json(&app, "POST", "/api/v1/probe/pause", json!({})).await;
    assert!(!state.probe.has_paused_exempt_work());

    let page1 = state
        .store()
        .probe_analysis_participants(show, 1, None, 16)
        .unwrap();
    assert_eq!(page1.len(), 16);
    let last_p1 = page1.last().unwrap().dispatch();
    let page2 = state
        .store()
        .probe_analysis_participants(
            show,
            1,
            Some((last_p1.job_id.as_str(), last_p1.ledger_id.as_str())),
            16,
        )
        .unwrap();
    assert_eq!(page2.len(), 2);
    let tail_ledger: domain::LedgerId = page2.last().unwrap().dispatch().ledger_id.parse().unwrap();
    let tail_row = state
        .store()
        .file_coverage_by_id(tail_ledger)
        .unwrap()
        .unwrap();

    state
        .probe
        .request_probe(&tail_row, ProbeRequestOrigin::Playback)
        .unwrap();
    assert!(state.probe.has_paused_exempt_work());

    let mut now = 1_000;
    poll_until(|| {
        now += 2_500;
        clock.set(now);
        state.probe.wake_dispatch();
        state
            .store()
            .active_probe_unit_for_ledger(&tail_ledger.to_string())
            .unwrap()
            .is_none()
    })
    .await;
    assert!(!state.probe.has_paused_exempt_work());
    assert!(state.probe.comparison_runs.load(Ordering::SeqCst) >= 2);
    state.probe.shutdown_workers().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_stops_waiting_buffered_workers_and_halts_pending_analysis_and_readonly_resume() {
    use api::probe_manager::policy::ProbeRequestOrigin;
    let dir = tempfile::tempdir().unwrap();
    let movie_root = dir.path().join("movies");
    let tv_root = dir.path().join("tv");
    std::fs::create_dir_all(&movie_root).unwrap();
    std::fs::create_dir_all(&tv_root).unwrap();
    let (movies, tv_eps) = {
        let store = api::Store::open(dir.path().join("data")).unwrap();
        create_lib(&store, MediaKind::Movie, "Movies", &movie_root, true);
        let movies: Vec<_> = (0..4)
            .map(|idx| seed_scanned_movie(&store, &movie_root, "buf", idx).1)
            .collect();
        let (_, tv_eps) = seed_tv_episodes(&store, &tv_root, 2);
        (movies, tv_eps)
    };
    let ro_state = production_state_with_writable(dir.path(), CountingTracks::new(false), false);
    let ro_req = common::request(
        "POST",
        "/api/v1/probe/resume",
        Some("management-secret"),
        json!({}),
    );
    assert_ne!(
        api::router(ro_state.clone())
            .oneshot(ro_req)
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert!(ro_state.probe.background_is_paused());

    let (tracks, entered_rx, release_tx) = CountingTracks::blocking_first();
    let state = production_state(dir.path(), tracks);
    let clock = Arc::new(api::probe_manager::clock::FakeClock::new(1_000));
    state.probe.set_clock(clock.clone());
    assert!(state.probe.try_start_workers());
    let app = api::router(state.clone());
    admin_json(&app, "POST", "/api/v1/probe/resume", json!({})).await;
    entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(
        admin_json(&app, "POST", "/api/v1/probe/pause", json!({})).await["paused"],
        true
    );
    release_tx.send(()).unwrap();
    poll_until(|| has_meta(&state, movies[0])).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    for &unstarted in &movies[1..] {
        assert!(!has_meta(&state, unstarted));
    }

    for &ep in &tv_eps {
        let row = state.store().file_coverage_by_id(ep).unwrap().unwrap();
        state
            .probe
            .request_probe(&row, ProbeRequestOrigin::Playback)
            .unwrap();
    }
    poll_until(|| tv_eps.iter().all(|&ep| in_analysis(&state, ep))).await;
    admin_json(&app, "POST", "/api/v1/probe/pause", json!({})).await;
    clock.set(25_000);
    state.probe.wake_dispatch();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(state.probe.comparison_runs.load(Ordering::SeqCst), 0);
    state.probe.shutdown_workers().await.unwrap();
}

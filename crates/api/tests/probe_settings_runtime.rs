#[path = "management/common.rs"]
mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

fn state(root: &std::path::Path) -> api::ApiState {
    common::state(
        root,
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(root.join("dl"))),
    )
}

async fn get(app: &axum::Router) -> Value {
    let response = app
        .clone()
        .oneshot(common::request(
            "GET",
            "/api/v1/settings/probe",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    common::json_data(response).await
}

async fn save(app: &axum::Router, value: Value) {
    let response = app
        .clone()
        .oneshot(common::request(
            "PUT",
            "/api/v1/settings/probe",
            Some("management-secret"),
            value.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(common::json_data(response).await, value);
}

const REMOTE_READS: &str = "/api/v1/settings/probe/remote-reads";

async fn remote_reads_request(app: &axum::Router, method: &str, body: Value) -> Value {
    let response = app
        .clone()
        .oneshot(common::request(
            method,
            REMOTE_READS,
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    common::json_data(response).await
}

async fn remote_pause_survives_restart(state_before: api::ApiState, root: &std::path::Path) {
    let path = root.join("remote.strm");
    let app = api::router(state_before.clone());
    assert_eq!(
        remote_reads_request(&app, "GET", Value::Null).await,
        json!({"paused": false})
    );
    let (_, token) = common::create_member(&app, "remote-reader").await;
    let response = app
        .clone()
        .oneshot(common::request(
            "PUT",
            REMOTE_READS,
            Some(&token),
            json!({"paused": true}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let active =
        marker::media_read::acquire_media_read(&path, &marker::WorkControl::default()).unwrap();
    assert_eq!(
        remote_reads_request(&app, "PUT", json!({"paused": true})).await,
        json!({"paused": true})
    );
    drop(active);
    state_before.probe.shutdown_workers().await.unwrap();

    // Starting an instance with default settings resets the process-wide gate.
    let other_dir = tempfile::tempdir().unwrap();
    let other = state(other_dir.path());
    other.probe.shutdown_workers().await.unwrap();
    let restarted = state(root);
    let app = api::router(restarted.clone());
    let paused = remote_reads_request(&app, "GET", Value::Null).await;
    let control = marker::WorkControl::default();
    let (entered_tx, mut entered) = tokio::sync::oneshot::channel();
    let waiting = tokio::task::spawn_blocking({
        let control = control.clone();
        move || {
            let _permit = marker::media_read::acquire_media_read(&path, &control)?;
            let _ = entered_tx.send(());
            Ok::<_, String>(())
        }
    });
    // A gate regression must fail promptly instead of leaving a blocking thread parked.
    let deadline = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        control.cancel();
    });
    let held = tokio::time::timeout(Duration::from_millis(100), &mut entered).await;
    marker::media_read::acquire_media_read(
        &root.join("local.mkv"),
        &marker::WorkControl::default(),
    )
    .unwrap();
    let stopped = waiting.await.unwrap().unwrap_err();
    deadline.abort();
    assert_eq!(stopped, marker::media_read::READ_WAIT_STOPPED);
    assert_eq!(
        remote_reads_request(&app, "PUT", json!({"paused": false})).await,
        json!({"paused": false})
    );
    assert_eq!(paused, json!({"paused": true}));
    assert!(
        !matches!(held, Ok(Ok(()))),
        "STRM read started while paused"
    );
    assert_eq!(stopped, marker::media_read::READ_WAIT_STOPPED);
    let (resumed_tx, resumed) = tokio::sync::oneshot::channel();
    let path = root.join("remote.strm");
    let read = tokio::task::spawn_blocking(move || {
        let permit =
            marker::media_read::acquire_media_read(&path, &marker::WorkControl::default())?;
        let _ = resumed_tx.send(());
        Ok::<_, String>(permit)
    });
    tokio::time::timeout(Duration::from_secs(1), resumed)
        .await
        .unwrap()
        .unwrap();
    drop(read.await.unwrap().unwrap());
    restarted.probe.shutdown_workers().await.unwrap();
}

async fn paused_metadata_requeues_on_shutdown(root: &std::path::Path) {
    let (first, _waiting, mut reads) = state_with_gated_tracks(root).await;
    let app = api::router(first.clone());
    remote_reads_request(&app, "PUT", json!({"paused": true})).await;
    let store = first.store();
    store
        .create_library(
            domain::MediaKind::Movie,
            "Remote",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let path = root.join("movie.strm");
    let row = queue_movie_source(
        &first,
        &store,
        &path,
        0,
        "https://fixture.invalid/video.mkv",
    );
    let ledger_id = row.file.file_id.to_string();
    let local = queue_movie(&first, &store, root, 1);
    let result = tokio::time::timeout(Duration::from_secs(2), reads.recv()).await;
    if result.is_err() {
        first.probe.shutdown_workers().await.unwrap();
    }
    let (started, release) = result
        .expect("local metadata blocked behind paused STRM")
        .unwrap();
    assert_eq!(started, local.file.path);
    release.send(()).unwrap();
    first.probe.shutdown_workers().await.unwrap();
    assert_eq!(
        store
            .active_probe_unit_for_ledger(&ledger_id)
            .unwrap()
            .unwrap()
            .status,
        "queued"
    );

    let (restarted, _waiting, mut reads) = state_with_gated_tracks(root).await;
    let app = api::router(restarted.clone());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), reads.recv())
            .await
            .is_err()
    );
    remote_reads_request(&app, "PUT", json!({"paused": false})).await;
    let (read_path, release) = tokio::time::timeout(Duration::from_secs(3), reads.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read_path, path);
    release.send(()).unwrap();
    restarted.probe.shutdown_workers().await.unwrap();
    assert!(store.get_file_meta(&ledger_id).unwrap().is_some());
}

async fn pausing_a_gate_waiter_releases_the_metadata_worker(root: &std::path::Path) {
    let (state, mut waiting, mut reads) = state_with_gated_tracks(root).await;
    let app = api::router(state.clone());
    let store = state.store();
    let occupied_gate = marker::media_read::acquire_media_read(
        &root.join("occupied.strm"),
        &marker::WorkControl::default(),
    )
    .unwrap();
    store
        .create_library(
            domain::MediaKind::Movie,
            "Remote",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let remote = queue_movie_source(
        &state,
        &store,
        &root.join("waiting.strm"),
        0,
        "https://fixture.invalid/video.mkv",
    );
    tokio::time::timeout(Duration::from_secs(2), waiting.recv())
        .await
        .unwrap()
        .unwrap();
    remote_reads_request(&app, "PUT", json!({"paused": true})).await;
    drop(occupied_gate);

    let local = queue_movie(&state, &store, root, 1);
    let (started, release) = tokio::time::timeout(Duration::from_secs(2), reads.recv())
        .await
        .expect("a paused STRM gate waiter must release the metadata slot")
        .unwrap();
    assert_eq!(started, local.file.path);
    release.send(()).unwrap();
    state.probe.shutdown_workers().await.unwrap();
    assert_eq!(
        store
            .active_probe_unit_for_ledger(&remote.file.file_id.to_string())
            .unwrap()
            .unwrap()
            .status,
        "queued",
        "pausing a pending remote read must preserve its durable work"
    );
}

async fn state_with_gated_tracks(
    root: &std::path::Path,
) -> (
    api::ApiState,
    tokio::sync::mpsc::UnboundedReceiver<()>,
    tokio::sync::mpsc::UnboundedReceiver<StartedRead>,
) {
    // Construct outside the runtime so the production engine cannot claim a
    // recovered unit before the fixture engine replaces it.
    let root = root.to_path_buf();
    let state = std::thread::spawn(move || state(&root)).join().unwrap();
    let (waiting, wait_rx) = tokio::sync::mpsc::unbounded_channel();
    let (reads, read_rx) = tokio::sync::mpsc::unbounded_channel();
    let state = state
        .with_probe_engines(
            Arc::new(marker::ChromaprintEngine),
            Arc::new(marker::ChromaprintEngine),
            Arc::new(GatedTracks {
                waiting,
                reads: HeldTracks(reads),
            }),
        )
        .unwrap();
    assert!(state.probe.try_start_workers());
    (state, wait_rx, read_rx)
}

async fn settings_roundtrip(app: &axum::Router) {
    assert_eq!(
        get(app).await,
        json!({
            "metadata_workers": 1, "remote_concurrency": 1, "remote_cooldown_ms": 500,
        })
    );
    let configured = json!({
        "metadata_workers": 3, "remote_concurrency": 2, "remote_cooldown_ms": 40,
    });
    save(app, configured.clone()).await;
    assert_eq!(get(app).await, configured);
    save(
        app,
        json!({"metadata_workers": 8, "remote_concurrency": 8, "remote_cooldown_ms": 60000}),
    )
    .await;
    save(app, configured.clone()).await;
    for (field, invalid) in [
        ("metadata_workers", json!(0)),
        ("metadata_workers", json!(9)),
        ("remote_concurrency", json!(0)),
        ("remote_concurrency", json!(9)),
        ("remote_cooldown_ms", json!(-1)),
        ("remote_cooldown_ms", json!(60001)),
        ("metadata_workers", json!(1.5)),
    ] {
        let mut body = configured.clone();
        body[field] = invalid;
        let response = app
            .clone()
            .oneshot(common::request(
                "PUT",
                "/api/v1/settings/probe",
                Some("management-secret"),
                body,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{field}");
        assert_eq!(
            common::json_body(response).await["error"]["code"],
            "probe.invalid"
        );
        assert_eq!(get(app).await, configured);
    }
    let (_, token) = common::create_member(app, "probe-member").await;
    for method in ["GET", "PUT"] {
        let response = app
            .clone()
            .oneshot(common::request(
                method,
                "/api/v1/settings/probe",
                Some(&token),
                configured.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}

async fn remote_admission_changes(app: &axum::Router, path: std::path::PathBuf) {
    save(
        app,
        json!({"metadata_workers": 1, "remote_concurrency": 1, "remote_cooldown_ms": 0}),
    )
    .await;
    let first =
        marker::media_read::acquire_media_read(&path, &marker::WorkControl::default()).unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let second_path = path.clone();
    let pending = tokio::task::spawn_blocking(move || {
        let _permit =
            marker::media_read::acquire_media_read(&second_path, &marker::WorkControl::default())
                .unwrap();
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    let mut entered = entered_rx;
    assert!(
        tokio::time::timeout(Duration::from_millis(60), &mut entered)
            .await
            .is_err()
    );
    save(
        app,
        json!({"metadata_workers": 1, "remote_concurrency": 2, "remote_cooldown_ms": 0}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(1), &mut entered)
        .await
        .unwrap()
        .unwrap();
    // Lowering the limit preserves both existing reads and blocks the next one.
    save(
        app,
        json!({"metadata_workers": 1, "remote_concurrency": 1, "remote_cooldown_ms": 40}),
    )
    .await;
    let (third_entered_tx, mut third_entered) = tokio::sync::oneshot::channel();
    let third = tokio::task::spawn_blocking(move || {
        let _permit =
            marker::media_read::acquire_media_read(&path, &marker::WorkControl::default()).unwrap();
        third_entered_tx.send(Instant::now()).unwrap();
    });
    drop(first);
    assert!(
        tokio::time::timeout(Duration::from_millis(60), &mut third_entered)
            .await
            .is_err()
    );
    release_tx.send(()).unwrap();
    pending.await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), third_entered)
        .await
        .unwrap()
        .unwrap();
    third.await.unwrap();
}

async fn default_remote_admission(path: std::path::PathBuf) {
    let first =
        marker::media_read::acquire_media_read(&path, &marker::WorkControl::default()).unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let pending = tokio::task::spawn_blocking(move || {
        let _permit =
            marker::media_read::acquire_media_read(&path, &marker::WorkControl::default()).unwrap();
        entered_tx.send(Instant::now()).unwrap();
    });
    let mut entered = entered_rx;
    assert!(
        tokio::time::timeout(Duration::from_millis(60), &mut entered)
            .await
            .is_err()
    );
    let released = Instant::now();
    drop(first);
    let started = tokio::time::timeout(Duration::from_secs(2), entered)
        .await
        .unwrap()
        .unwrap();
    assert!(started.duration_since(released) >= Duration::from_millis(500));
    pending.await.unwrap();
}

type StartedRead = (std::path::PathBuf, std::sync::mpsc::Sender<()>);

struct HeldTracks(tokio::sync::mpsc::UnboundedSender<StartedRead>);

struct GatedTracks {
    waiting: tokio::sync::mpsc::UnboundedSender<()>,
    reads: HeldTracks,
}

impl api::probe_manager::TracksProbe for GatedTracks {
    fn probe(
        &self,
        path: &std::path::Path,
        control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        self.waiting.send(()).unwrap();
        let _permit =
            marker::media_read::acquire_media_read(path, control.signal()).map_err(|error| {
                if error == marker::media_read::READ_WAIT_STOPPED {
                    library::LibraryError::ReadWaitStopped
                } else {
                    library::LibraryError::Probe(error)
                }
            })?;
        self.reads.probe(path, control)
    }
}

impl api::probe_manager::TracksProbe for HeldTracks {
    fn probe(
        &self,
        path: &std::path::Path,
        _: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        let (release, wait) = std::sync::mpsc::channel();
        self.0.send((path.to_path_buf(), release)).unwrap();
        wait.recv_timeout(Duration::from_secs(3)).unwrap();
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack::default()),
                ..Default::default()
            },
            Some(60_000),
        ))
    }
}

fn queue_movies(
    state: &api::ApiState,
    root: &std::path::Path,
    count: usize,
) -> Vec<domain::FileCoverage> {
    let store = state.store();
    store
        .create_library(
            domain::MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    (0..count)
        .map(|index| queue_movie(state, &store, root, index))
        .collect()
}

fn queue_movie(
    state: &api::ApiState,
    store: &api::Store,
    root: &std::path::Path,
    index: usize,
) -> domain::FileCoverage {
    let path = root.join(format!("movie-{index}.mkv"));
    queue_movie_source(state, store, &path, index, "fixture")
}

fn queue_movie_source(
    state: &api::ApiState,
    store: &api::Store,
    path: &std::path::Path,
    index: usize,
    source: &str,
) -> domain::FileCoverage {
    std::fs::write(path, source).unwrap();
    let media_id = domain::MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: domain::MediaKind::Movie,
            title: format!("Movie {index}"),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let file = store::test_support::publish_fixture_file(
        store,
        &store::test_support::FixtureFile {
            media_id,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let row = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    state
        .probe
        .request_probe(
            &row,
            api::probe_manager::policy::ProbeRequestOrigin::ManualRefresh,
        )
        .unwrap();
    row
}

async fn metadata_worker_changes(
    app: &axum::Router,
    state: &api::ApiState,
    root: &std::path::Path,
    reads: &mut tokio::sync::mpsc::UnboundedReceiver<StartedRead>,
) {
    let rows = queue_movies(state, root, 4);
    let (first_path, first_release) = tokio::time::timeout(Duration::from_secs(1), reads.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(60), reads.recv())
            .await
            .is_err(),
        "default worker count must serialize metadata"
    );
    save(
        app,
        json!({"metadata_workers": 2, "remote_concurrency": 1, "remote_cooldown_ms": 500}),
    )
    .await;
    let (_, second_release) = tokio::time::timeout(Duration::from_secs(1), reads.recv())
        .await
        .unwrap()
        .unwrap();
    save(
        app,
        json!({"metadata_workers": 1, "remote_concurrency": 1, "remote_cooldown_ms": 500}),
    )
    .await;
    first_release.send(()).unwrap();
    let first = rows.iter().find(|row| row.file.path == first_path).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while state
            .store()
            .get_file_meta(&first.file.file_id.to_string())
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let premature = tokio::time::timeout(Duration::from_millis(80), reads.recv()).await;
    second_release.send(()).unwrap();
    let exceeded = premature.is_ok();
    if let Ok(Some((_, release))) = premature {
        release.send(()).unwrap();
    }
    for _ in 0..if exceeded { 1 } else { 2 } {
        let (_, release) = tokio::time::timeout(Duration::from_secs(1), reads.recv())
            .await
            .unwrap()
            .unwrap();
        release.send(()).unwrap();
    }
    assert!(
        !exceeded,
        "lowering worker count must wait for existing reads before admitting another file"
    );
}

async fn older_metadata_waiters_start_before_newer_queued_files() {
    let root = tempfile::tempdir().unwrap();
    let (started, mut reads) = tokio::sync::mpsc::unbounded_channel();
    let state = tokio::task::spawn_blocking({
        let root = root.path().to_path_buf();
        move || state(&root)
    })
    .await
    .unwrap();
    state.probe.shutdown_workers().await.unwrap();
    let state = state
        .with_probe_engines(
            Arc::new(marker::ChromaprintEngine),
            Arc::new(marker::ChromaprintEngine),
            Arc::new(HeldTracks(started)),
        )
        .unwrap();
    assert!(state.probe.try_start_workers());
    let older = queue_movies(&state, root.path(), 8);
    let (first_path, first_release) = tokio::time::timeout(Duration::from_secs(1), reads.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.probe.queued_ledger_count(), 8);
    tokio::time::sleep(Duration::from_millis(80)).await;
    let late = queue_movie(&state, &state.store(), root.path(), 8);
    tokio::time::sleep(Duration::from_millis(80)).await;

    first_release.send(()).unwrap();
    let mut order = vec![first_path];
    for _ in 1..9 {
        let (path, release) = tokio::time::timeout(Duration::from_secs(2), reads.recv())
            .await
            .unwrap()
            .unwrap();
        order.push(path);
        release.send(()).unwrap();
    }
    state.probe.shutdown_workers().await.unwrap();

    let late_position = order
        .iter()
        .position(|path| path == &late.file.path)
        .unwrap();
    assert_eq!(
        late_position, 8,
        "newer file must follow all eight older queued files: {order:?}"
    );
    assert_eq!(
        order
            .iter()
            .filter(|path| older.iter().any(|row| &row.file.path == *path))
            .count(),
        8
    );
    assert_eq!(
        order,
        older
            .iter()
            .map(|row| row.file.path.clone())
            .chain([late.file.path])
            .collect::<Vec<_>>(),
        "metadata must retain dispatch order with one active read"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_settings_persist_validate_and_apply_without_restart() {
    let _serial = PROCESS_GATE.lock().await;
    // Both scenarios use process-wide remote settings, so keep them within one
    // test instead of allowing parallel runtimes to change each other's gate.
    older_metadata_waiters_start_before_newer_queued_files().await;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("remote.strm");
    std::fs::write(&path, "https://fixture.invalid/video.mkv").unwrap();
    let (started, mut reads) = tokio::sync::mpsc::unbounded_channel();
    let first_state = tokio::task::spawn_blocking({
        let root = root.path().to_path_buf();
        move || state(&root)
    })
    .await
    .unwrap();
    first_state.probe.shutdown_workers().await.unwrap();
    let first_state = first_state
        .with_probe_engines(
            Arc::new(marker::ChromaprintEngine),
            Arc::new(marker::ChromaprintEngine),
            Arc::new(HeldTracks(started)),
        )
        .unwrap();
    assert!(first_state.probe.try_start_workers());
    default_remote_admission(path.clone()).await;
    let app = api::router(first_state.clone());
    metadata_worker_changes(&app, &first_state, root.path(), &mut reads).await;
    settings_roundtrip(&app).await;
    remote_admission_changes(&app, path.clone()).await;
    first_state.probe.shutdown_workers().await.unwrap();
    let restarted = state(root.path());
    assert_eq!(
        get(&api::router(restarted.clone())).await,
        json!({
            "metadata_workers": 1, "remote_concurrency": 1, "remote_cooldown_ms": 40,
        })
    );
    assert!(restarted.probe.try_start_workers());
    let released = tokio::task::spawn_blocking({
        let path = path.clone();
        move || {
            drop(
                marker::media_read::acquire_media_read(&path, &marker::WorkControl::default())
                    .unwrap(),
            );
            Instant::now()
        }
    })
    .await
    .unwrap();
    tokio::task::spawn_blocking(move || {
        let _permit =
            marker::media_read::acquire_media_read(&path, &marker::WorkControl::default()).unwrap();
        assert!(released.elapsed() >= Duration::from_millis(40));
        assert!(
            released.elapsed() < Duration::from_millis(400),
            "startup must load the saved cooldown"
        );
    })
    .await
    .unwrap();
    remote_pause_survives_restart(restarted, root.path()).await;
}

static PROCESS_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_remote_jobs_allow_local_metadata() {
    let _serial = PROCESS_GATE.lock().await;
    paused_metadata_requeues_on_shutdown(tempfile::tempdir().unwrap().path()).await;
    pausing_a_gate_waiter_releases_the_metadata_worker(tempfile::tempdir().unwrap().path()).await;
}

#[path = "probe_settings_runtime/pause_requeue.rs"]
mod pause_requeue;

#[path = "probe_settings_runtime/fingerprint_order.rs"]
mod fingerprint_order;
#[path = "probe_settings_runtime/stale_startup.rs"]
mod stale_startup;
#[path = "probe_settings_runtime/standby.rs"]
mod standby;

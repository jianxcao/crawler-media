use super::*;
use std::sync::mpsc::{Receiver, Sender};

struct DeferredAfterPause {
    entered_probe: Sender<()>,
    paused_applied: std::sync::Mutex<Receiver<()>>,
    reached_gate: Sender<()>,
    resume: std::sync::Mutex<Receiver<()>>,
    calls: std::sync::atomic::AtomicUsize,
    completed: Sender<()>,
}

impl api::probe_manager::TracksProbe for DeferredAfterPause {
    fn probe(
        &self,
        path: &std::path::Path,
        control: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        assert_eq!(
            path.extension().and_then(|value| value.to_str()),
            Some("strm"),
            "{path:?}"
        );
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            self.entered_probe.send(()).unwrap();
            self.paused_applied
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            assert!(marker::media_read::remote_reads_paused());
            let error = match marker::media_read::acquire_media_read(path, control.signal()) {
                Ok(_permit) => panic!("the request was paused before it entered the gate"),
                Err(error) => error,
            };
            assert_eq!(error, marker::media_read::READ_WAIT_STOPPED);
            self.reached_gate.send(()).unwrap();
            self.resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            return Err(library::LibraryError::ReadWaitStopped);
        }
        self.completed.send(()).unwrap();
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack::default()),
                ..Default::default()
            },
            Some(60_000),
        ))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn immediate_resume_does_not_turn_a_stopped_strm_probe_into_failure() {
    let _serial = PROCESS_GATE.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let base = std::thread::spawn({
        let root = root.clone();
        move || state(&root)
    })
    .join()
    .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (paused_tx, paused_rx) = std::sync::mpsc::channel();
    let (reached_tx, reached_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    let state = base
        .with_probe_engines(
            Arc::new(marker::ChromaprintEngine),
            Arc::new(marker::ChromaprintEngine),
            Arc::new(DeferredAfterPause {
                entered_probe: entered_tx,
                paused_applied: std::sync::Mutex::new(paused_rx),
                reached_gate: reached_tx,
                resume: std::sync::Mutex::new(resume_rx),
                calls: std::sync::atomic::AtomicUsize::new(0),
                completed: completed_tx,
            }),
        )
        .unwrap();
    assert!(state.probe.try_start_workers());
    let app = api::router(state.clone());

    let store = state.store();
    let library_root = root.join("movies");
    std::fs::create_dir_all(&library_root).unwrap();
    store
        .create_library(
            domain::MediaKind::Movie,
            "Movies",
            &[library_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let row = queue_movie_source(
        &state,
        &store,
        &library_root.join("paused.strm"),
        0,
        "https://fixture.invalid/movie.mkv",
    );
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(2)).unwrap())
        .await
        .unwrap();
    assert_eq!(
        remote_reads_request(&app, "PUT", json!({"paused": true})).await,
        json!({"paused": true})
    );
    paused_tx.send(()).unwrap();
    tokio::task::spawn_blocking(move || reached_rx.recv_timeout(Duration::from_secs(2)).unwrap())
        .await
        .unwrap();

    remote_reads_request(&app, "PUT", json!({"paused": false})).await;
    resume_tx.send(()).unwrap();
    tokio::task::spawn_blocking(move || completed_rx.recv_timeout(Duration::from_secs(2)).unwrap())
        .await
        .unwrap();
    let ledger_id = row.file.file_id.to_string();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if store.get_file_meta(&ledger_id).unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the resumed probe must publish its stream details");
    state.probe.shutdown_workers().await.unwrap();
}

fn publish_strm_file(
    store: &api::Store,
    kind: domain::MediaKind,
    title: &str,
    path: &std::path::Path,
    season_ep: Option<(u32, u32)>,
) -> domain::FileCoverage {
    std::fs::write(path, "https://fixture.invalid/strm.mkv").unwrap();
    let media_id = domain::MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind,
            title: title.into(),
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
    let (season, episode) = season_ep.map_or((None, None), |(s, e)| (Some(s), Some(e)));
    let file = store::test_support::publish_fixture_file(
        store,
        &store::test_support::FixtureFile {
            media_id,
            path: path.display().to_string(),
            season,
            episode,
            ..Default::default()
        },
    )
    .unwrap();
    store.file_coverage_by_id(file.file_id).unwrap().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paused_remote_reads_keep_strm_probes_out_of_dispatch_channels() {
    let _serial = PROCESS_GATE.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let base = std::thread::spawn({
        let root = root.clone();
        move || state(&root)
    })
    .join()
    .unwrap();
    let app = api::router(base.clone());
    assert_eq!(
        remote_reads_request(&app, "PUT", json!({"paused": true})).await,
        json!({"paused": true})
    );
    let store = base.store();
    let movies = root.join("movies");
    let shows = root.join("shows");
    std::fs::create_dir_all(&movies).unwrap();
    std::fs::create_dir_all(&shows).unwrap();
    store
        .create_library(
            domain::MediaKind::Movie,
            "Movies",
            &[movies.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let tv_lib = store
        .create_library(
            domain::MediaKind::Tv,
            "Shows",
            &[shows.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .set_library_intro_settings(&tv_lib.id, true, true)
        .unwrap();

    let meta_row = queue_movie_source(
        &base,
        &store,
        &movies.join("meta.strm"),
        0,
        "https://fixture.invalid/meta.mkv",
    );
    let force_row = publish_strm_file(
        &store,
        domain::MediaKind::Movie,
        "Force",
        &movies.join("force.strm"),
        None,
    );
    assert!(base.probe.enqueue_force(api::probe_manager::ProbeUnit {
        row: force_row.clone(),
        kind: domain::MediaKind::Movie,
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: false,
        marker_refresh_id: None,
        job_id: None,
    }));
    let ep_row = publish_strm_file(
        &store,
        domain::MediaKind::Tv,
        "Show",
        &shows.join("Show.S01E01.strm"),
        Some((1, 1)),
    );
    assert!(
        base.probe
            .enqueue_direct_fingerprint(
                api::probe_manager::ProbeUnit {
                    row: ep_row.clone(),
                    kind: domain::MediaKind::Tv,
                    force_fingerprint: true,
                    reuse_fingerprint_cache: true,
                    overwrite_markers: false,
                    reuse_media_info_cache: true,
                    marker_refresh_id: None,
                    job_id: None,
                },
                Some(120_000),
                ep_row.file.version.source_version.clone(),
                180,
            )
            .is_some()
    );
    assert_eq!(
        base.probe.queued_ledger_count(),
        0,
        "paused remote reads must keep STRM units out of in-memory dispatch channels"
    );
    for id in [
        meta_row.file.file_id,
        force_row.file.file_id,
        ep_row.file.file_id,
    ] {
        assert_eq!(
            store
                .active_probe_unit_for_ledger(&id.to_string())
                .unwrap()
                .unwrap()
                .status,
            "queued"
        );
    }
    remote_reads_request(&app, "PUT", json!({"paused": false})).await;
    base.probe.shutdown_workers().await.unwrap();
}

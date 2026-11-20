use super::*;
use std::sync::mpsc::{Receiver, Sender};

struct DeferredAfterPause {
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
    let base_app = api::router(base.clone());
    assert_eq!(
        remote_reads_request(&base_app, "PUT", json!({"paused": true})).await,
        json!({"paused": true})
    );
    let (reached_tx, reached_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    let state = base
        .with_probe_engines(
            Arc::new(marker::ChromaprintEngine),
            Arc::new(marker::ChromaprintEngine),
            Arc::new(DeferredAfterPause {
                reached_gate: reached_tx,
                resume: std::sync::Mutex::new(resume_rx),
                calls: std::sync::atomic::AtomicUsize::new(0),
                completed: completed_tx,
            }),
        )
        .unwrap();
    assert!(state.probe.try_start_workers());
    let app = api::router(state.clone());
    assert!(marker::media_read::remote_reads_paused());

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
    assert!(
        marker::media_read::remote_reads_paused(),
        "Library setup reset pause"
    );
    let row = queue_movie_source(
        &state,
        &store,
        &library_root.join("paused.strm"),
        0,
        "https://fixture.invalid/movie.mkv",
    );
    assert!(
        marker::media_read::remote_reads_paused(),
        "request enqueue reset pause"
    );
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

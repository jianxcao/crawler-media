//! A delayed wash-cut deletion cannot acquire a replacement source or new Media owner.
use super::harness::*;
use api::file_stat_read::FileStatRead;
use domain::{LibraryFile, SubscribeId};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use store::library_write::{ReassignFiles, WriteScope};

struct PausedIdentity {
    path: PathBuf,
    entered: tokio::sync::Notify,
    once: AtomicBool,
    resume: parking_lot::Mutex<mpsc::Receiver<()>>,
}
impl FileStatRead for PausedIdentity {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        library::read_nfo(path)
    }
    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
    fn symlink_metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        let observed = std::fs::symlink_metadata(path);
        if path == self.path && !self.once.swap(true, Ordering::SeqCst) {
            self.entered.notify_one();
            self.resume
                .lock()
                .recv_timeout(Duration::from_secs(10))
                .expect("release deletion identity observation");
        }
        observed
    }
}

struct ReadyDownloader(parking_lot::Mutex<Vec<PathBuf>>);
impl downloader::Downloader for ReadyDownloader {
    fn add(&self, _: &domain::Torrent) -> Result<(), downloader::DownloaderError> {
        Ok(())
    }
    fn completed_files(
        &self,
        _: &domain::Torrent,
    ) -> Result<Vec<PathBuf>, downloader::DownloaderError> {
        Ok(self.0.lock().clone())
    }
}

async fn prepared(manual: bool) -> (Harness, SubscribeId, LibraryFile) {
    let ready = Arc::new(ReadyDownloader(Default::default()));
    let h = if manual {
        Harness::with_downloader(ready.clone())
    } else {
        Harness::new()
    };
    h.create_site().await;
    let id = h.create_movie_subscribe(json!({"wash_cut":true})).await;
    let owned = h.seed_owned_movie(id, 1080, "h264", 1);
    let video = probed_video(2160, "hevc", false);
    if manual {
        let source = h
            .tmp
            .path()
            .join("ready")
            .join(format!("{MATRIX_TITLE}.mkv"));
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, video).unwrap();
        *ready.0.lock() = vec![source];
    } else {
        h.run_subscribe(id).await;
        h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &video)]);
    }
    let file = h
        .state
        .store()
        .lock()
        .file_coverage_by_path(owned.to_str().unwrap())
        .unwrap()
        .unwrap()
        .file;
    (h, id, file)
}

fn change_owned(h: &Harness, file: &LibraryFile, reassign: bool) -> LibraryFile {
    let shared = h.state.store();
    let store = shared
        .try_lock_for(Duration::from_millis(200))
        .expect("identity IO must release Store");
    if reassign {
        let mut target = store.get_media(file.media_id).unwrap().unwrap();
        target.id = domain::MediaId::new();
        target.title = "Replacement Media owner".into();
        target.aliases.clear();
        target.tmdb_id = None;
        store.insert_media(&target).unwrap();
        store
            .reassign_files(
                &ReassignFiles {
                    files: vec![(file.file_id, file.version.clone())],
                    from: file.media_id,
                    to: target.id,
                    expected_media_revision: store.media_revision(file.media_id).unwrap().unwrap(),
                },
                1000,
            )
            .unwrap();
    } else {
        let mut bytes = std::fs::read(&file.path).unwrap();
        bytes[0] ^= 1;
        // Same inode and size: path-only deletion evidence cannot distinguish this generation.
        std::fs::write(&file.path, bytes).unwrap();
        let observation = library::observe_source(&file.path).unwrap();
        store
            .replace_file_source(
                &WriteScope {
                    library_id: file.library_id,
                    config_revision: store
                        .library_config_revision(&file.library_id.to_string())
                        .unwrap(),
                },
                file.file_id,
                &file.version,
                &observation,
                1000,
            )
            .unwrap();
    }
    store
        .file_coverage_by_id(file.file_id)
        .unwrap()
        .unwrap()
        .file
}

async fn delayed_deletion(manual: bool, reassign: bool) {
    let (mut h, id, original) = prepared(manual).await;
    let (resume, receiver) = mpsc::channel();
    let pause = Arc::new(PausedIdentity {
        path: original.path.clone(),
        entered: tokio::sync::Notify::new(),
        once: AtomicBool::new(false),
        resume: parking_lot::Mutex::new(receiver),
    });
    h.state = h.state.with_file_stat_read(pause.clone());
    h.app = api::router(h.state.clone());
    let state = h.state.clone();
    let app = h.app.clone();
    let mut attempt = tokio::spawn(async move {
        if manual {
            use tower::ServiceExt;
            let response = app
                .oneshot(super::common::request(
                    "POST",
                    &format!("/api/v1/subscriptions/{id}/run"),
                    Some(TOKEN),
                    serde_json::Value::Null,
                ))
                .await
                .unwrap();
            let status = response.status();
            format!("{status} {}", super::common::json_body(response).await)
        } else {
            format!(
                "{:?}",
                tokio::task::spawn_blocking(move || api::worker::transfer_one(&state, id))
                    .await
                    .unwrap()
            )
        }
    });
    tokio::select! {
        waited = tokio::time::timeout(Duration::from_secs(5),pause.entered.notified()) => waited.expect("owned deletion identity observed"),
        ended = &mut attempt => panic!("run finished before owned deletion observation: {ended:?}"),
    }
    let updated = change_owned(&h, &original, reassign);
    let expected_bytes = std::fs::read(&original.path).unwrap();
    resume.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), attempt)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read(&original.path).expect("replacement bytes survive"),
        expected_bytes,
        "{result}"
    );
    let shared = h.state.store();
    let store = shared.lock();
    let current = store
        .file_coverage_by_id(original.file_id)
        .unwrap()
        .expect("replacement row survives")
        .file;
    assert_eq!(
        current, updated,
        "original snapshot cannot mutate the replacement: {result}"
    );
    assert!(
        store.list_pending_file_deletions().unwrap().is_empty(),
        "stale deletion never queued: {result}"
    );
}

#[tokio::test]
async fn manual_run_does_not_delete_a_replaced_owned_source() {
    delayed_deletion(true, false).await;
}
#[tokio::test]
async fn scheduled_transfer_does_not_delete_a_replaced_owned_source() {
    delayed_deletion(false, false).await;
}
#[tokio::test]
async fn manual_run_does_not_delete_an_owned_file_reassigned_to_another_media() {
    delayed_deletion(true, true).await;
}
#[tokio::test]
async fn scheduled_transfer_does_not_delete_an_owned_file_reassigned_to_another_media() {
    delayed_deletion(false, true).await;
}

//! Public Transfer completion must preserve a concurrent Subscribe PATCH.
use super::support::*;
use axum::http::StatusCode;
use downloader::{Downloader, DownloaderError};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

struct CompletionDownloader {
    observed: Arc<AtomicBool>,
}
impl Downloader for CompletionDownloader {
    fn add(&self, _: &domain::Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }
    fn completed_files(&self, _: &domain::Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.observed.store(true, Ordering::Release);
        Ok(Vec::new())
    }
}

struct CompletionFileRead {
    observed: Arc<AtomicBool>,
    blocked: AtomicBool,
    path: PathBuf,
    gate: Arc<Gate>,
    after_completed: Option<(Arc<api::Store>, domain::SubscribeId)>,
}
impl api::file_stat_read::FileStatRead for CompletionFileRead {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        library::read_nfo(path)
    }
    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
    fn is_file(&self, path: &Path) -> bool {
        // Before collectable_files, Transfer observes its input facts. With no
        // newly placed files, its next existence observation is completion.
        let phase_ready = self.after_completed.as_ref().is_none_or(|(store, id)| {
            store
                .get_subscribe(*id)
                .unwrap()
                .is_some_and(|subscribe| subscribe.tracking_state == "completed")
        });
        if path == self.path
            && phase_ready
            && self.observed.load(Ordering::Acquire)
            && !self.blocked.swap(true, Ordering::AcqRel)
        {
            self.gate.block();
        }
        path.is_file()
    }
}

#[test]
fn transfer_completion_observation_does_not_overwrite_pause() {
    assert_pause_preserved(false);
}

#[test]
fn transfer_completion_notice_observation_does_not_publish_after_pause() {
    assert_pause_preserved(true);
}

fn assert_pause_preserved(after_completed: bool) {
    let observed = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(Gate::default());
    let mut fixture = Fixture::new(Arc::new(CompletionDownloader {
        observed: observed.clone(),
    }));
    let path = fixture.root.join("The.Matrix.1999.mkv");
    std::fs::write(&path, b"complete local Library fixture").unwrap();
    store::test_support::publish_fixture_file(
        &fixture.state.store(),
        &store::test_support::FixtureFile {
            media_id: fixture.media,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    fixture.pending();
    let after_completed = after_completed.then(|| (fixture.state.store(), fixture.subscribe));
    fixture.state = fixture
        .state
        .with_file_stat_read(Arc::new(CompletionFileRead {
            observed,
            blocked: AtomicBool::new(false),
            path,
            gate: gate.clone(),
            after_completed,
        }));
    let _release = ReleaseOnDrop(gate.clone());
    runtime().block_on(async {
        let state = fixture.state.clone();
        let id = fixture.subscribe;
        let transfer = tokio::task::spawn_blocking(move || api::worker::transfer_one(&state, id));
        gate.entered().await;
        let mutation = tokio::time::timeout(
            SLO,
            fixture.request(
                "PATCH",
                &format!("/api/v1/subscriptions/{id}"),
                json!({"tracking_state": "paused"}),
            ),
        )
        .await;
        gate.release();
        transfer.await.unwrap().unwrap();
        let (status, body) =
            mutation.expect("completion disk observation must release Subscribe guard");
        assert_eq!(status, StatusCode::OK, "{body}");
        let store = fixture.state.store();
        assert_eq!(
            store.get_subscribe(id).unwrap().unwrap().tracking_state,
            "paused",
            "completion observed before PATCH cannot overwrite its pause"
        );
        assert!(
            store
                .get_setting(&format!("subscribe.completed:{id}"))
                .unwrap()
                .is_none(),
            "stale completion must not publish a completion notice marker"
        );
    });
}

#[path = "support/mod.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use axum::http::StatusCode;
use domain::LibraryFile;
use store::test_support::{FixtureFile, publish_fixture_file};
use support::library_presence::PresenceFixture;

struct Fixture {
    presence: PresenceFixture,
    first: LibraryFile,
    second: LibraryFile,
}

impl Fixture {
    fn new() -> Self {
        let presence = PresenceFixture::new();
        std::fs::write(presence.root.join("first.mkv"), b"first copy").unwrap();
        std::fs::write(presence.root.join("second.mkv"), b"second copy").unwrap();
        let (media_id, first) = presence.insert_media_and_ledger("first.mkv");
        let second = publish_fixture_file(
            &presence.state.store(),
            &FixtureFile {
                media_id,
                path: presence.root.join("second.mkv").display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        Self {
            presence,
            first,
            second,
        }
    }

    fn queue_first(&self) {
        queue_deletion(&self.presence.state.store(), &self.first.path);
    }

    fn delete_second(&self) -> StatusCode {
        let uri = format!(
            "/api/v1/libraries/{}/duplicates/{}",
            self.presence.library_id, self.second.file_id,
        );
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(
                self.presence
                    .request("DELETE", &uri, Some(&self.presence.admin_token)),
            )
            .0
    }

    fn assert_both_kept(&self) {
        assert_eq!(std::fs::read(&self.first.path).unwrap(), b"first copy");
        assert_eq!(std::fs::read(&self.second.path).unwrap(), b"second copy");
        let store = self.presence.state.store();
        assert!(
            store
                .file_coverage_by_id(self.second.file_id)
                .unwrap()
                .is_some()
        );
        assert!(
            !store
                .file_deletion_pending(self.second.path.to_str().unwrap())
                .unwrap()
        );
        assert!(
            store
                .file_deletion_pending(self.first.path.to_str().unwrap())
                .unwrap()
        );
    }
}

fn queue_deletion(store: &store::Store, path: &Path) {
    let path = path.to_str().unwrap();
    store
        .enqueue_file_deletion_observed(path, "duplicate", store::file_identity(path))
        .unwrap();
}

#[test]
fn a_copy_already_pending_deletion_cannot_protect_the_last_copy() {
    let fixture = Fixture::new();
    fixture.queue_first();

    assert_eq!(fixture.delete_second(), StatusCode::BAD_REQUEST);

    fixture.assert_both_kept();
}

struct QueueDuringObservation {
    store: Arc<store::Store>,
    queued: AtomicBool,
    pending_path: PathBuf,
}

impl api::file_stat_read::FileStatRead for QueueDuringObservation {
    fn read_nfo(&self, _: &Path) -> Option<library::NfoMeta> {
        None
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        let metadata = std::fs::metadata(path)?;
        if !self.queued.swap(true, Ordering::SeqCst) {
            queue_deletion(&self.store, &self.pending_path);
        }
        Ok(metadata)
    }
}

#[test]
fn a_copy_queued_during_observation_cannot_protect_the_last_copy() {
    let mut fixture = Fixture::new();
    let files = Arc::new(QueueDuringObservation {
        store: fixture.presence.state.store(),
        queued: AtomicBool::new(false),
        pending_path: fixture.first.path.clone(),
    });
    fixture.presence.state = fixture
        .presence
        .state
        .clone()
        .with_file_stat_read(files.clone());

    assert_eq!(fixture.delete_second(), StatusCode::BAD_REQUEST);

    assert!(files.queued.load(Ordering::SeqCst));
    fixture.assert_both_kept();
}

#[test]
fn a_pending_deletion_does_not_block_deletion_with_another_retained_copy() {
    let fixture = Fixture::new();
    let third = fixture.presence.root.join("third.mkv");
    std::fs::write(&third, b"retained copy").unwrap();
    publish_fixture_file(
        &fixture.presence.state.store(),
        &FixtureFile {
            media_id: fixture.first.media_id,
            path: third.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    fixture.queue_first();

    assert_eq!(fixture.delete_second(), StatusCode::OK);

    assert!(!fixture.second.path.exists());
    assert_eq!(std::fs::read(&third).unwrap(), b"retained copy");
    assert_eq!(std::fs::read(&fixture.first.path).unwrap(), b"first copy");
}

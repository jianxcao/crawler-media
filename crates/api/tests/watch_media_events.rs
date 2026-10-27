#[path = "support/mod.rs"]
mod support;

use std::path::Path;
use std::sync::Arc;

use api::fs_watcher::{StrmGraceTracker, handle_fs_events};
use notify_debouncer_mini::{DebouncedEvent, DebouncedEventKind};
use support::library_presence::PresenceFixture;

fn event(path: impl AsRef<Path>) -> DebouncedEvent {
    DebouncedEvent::new(path.as_ref().to_path_buf(), DebouncedEventKind::Any)
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

async fn baseline(fix: &PresenceFixture) -> (std::path::PathBuf, Arc<StrmGraceTracker>) {
    let media_dir = fix.root.join("Test Movie (2024)");
    std::fs::create_dir_all(&media_dir).unwrap();
    let media = media_dir.join("Test Movie (2024).strm");
    std::fs::write(&media, "https://example.invalid/first.mkv").unwrap();
    fix.scan_and_wait_completed(&fix.library_id).await;
    let tracker = Arc::new(StrmGraceTracker::new());
    tracker.on_created_or_modified(&media, Some("https://example.invalid/first.mkv"));
    (media, tracker)
}

fn operations(fix: &PresenceFixture) -> Vec<store::ScanOperation> {
    fix.state
        .store()
        .lock()
        .scan_operations_for_library(&fix.library_id)
        .unwrap()
}

#[test]
fn sidecar_and_parent_directory_events_do_not_start_another_scan() {
    // Construct outside Tokio: no ffprobe/fingerprint workers or network are started.
    let fix = PresenceFixture::new();
    runtime().block_on(async {
        let (media, tracker) = baseline(&fix).await;
        let media_dir = media.parent().unwrap();
        let before = operations(&fix);
        let nfo = media.with_extension("nfo");
        let thumb = media_dir.join("Test Movie (2024)-thumb.jpg");
        std::fs::write(&nfo, b"<movie/>").unwrap();
        std::fs::write(&thumb, b"jpeg").unwrap();
        let staging = media_dir.join(".frame-staging-123-0");
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("frame.jpg"), b"jpeg").unwrap();
        handle_fs_events(
            &fix.state,
            &tracker,
            vec![event(nfo), event(thumb), event(&staging), event(media_dir)],
        );
        std::fs::remove_dir_all(&staging).unwrap();
        handle_fs_events(&fix.state, &tracker, vec![event(staging), event(media_dir)]);
        let after = operations(&fix);
        assert_eq!(
            after.len(),
            before.len(),
            "generated files must not schedule a scan"
        );
        assert_eq!(after[0].dirty_rev, before[0].dirty_rev);
    });
}

#[test]
fn directory_only_event_ingests_a_new_strm_and_then_settles() {
    let fix = PresenceFixture::new();
    runtime().block_on(async {
        let (_, tracker) = baseline(&fix).await;
        let new_dir = fix.root.join("New Movie (2025)");
        std::fs::create_dir(&new_dir).unwrap();
        let new_media = new_dir.join("New Movie (2025).strm");
        std::fs::write(&new_media, "https://example.invalid/new.mkv").unwrap();
        handle_fs_events(&fix.state, &tracker, vec![event(&fix.root)]);
        let op = operations(&fix)
            .into_iter()
            .find(|op| op.status.is_active())
            .unwrap();
        let done = fix.wait_scan_terminal_op(&op.id).await;
        assert_eq!(done.status, store::ScanOperationStatus::Completed);
        assert!(
            fix.state
                .store()
                .lock()
                .file_coverage_by_path(new_media.to_str().unwrap())
                .unwrap()
                .is_some()
        );
        let count = operations(&fix).len();
        handle_fs_events(
            &fix.state,
            &tracker,
            vec![event(&fix.root), event(&new_dir)],
        );
        assert_eq!(operations(&fix).len(), count, "unchanged media must settle");
    });
}

#[test]
fn directory_only_deletion_and_restore_preserve_the_ledger_and_user_facts() {
    let fix = PresenceFixture::new();
    runtime().block_on(async {
        let (media, tracker) = baseline(&fix).await;
        let row = fix
            .state
            .store()
            .lock()
            .file_coverage_by_path(media.to_str().unwrap())
            .unwrap()
            .expect("scanned STRM is a recorded file");
        let user = domain::UserId::new();
        fix.state
            .store()
            .lock()
            .set_playback_progress(user, row.file.media_id, 12_345)
            .unwrap();
        std::fs::remove_file(&media).unwrap();
        handle_fs_events(&fix.state, &tracker, vec![event(media.parent().unwrap())]);
        assert!(
            fix.state
                .store()
                .lock()
                .missing_at_by_path()
                .unwrap()
                .contains_key(&row.path_key)
        );
        std::fs::write(&media, "https://example.invalid/first.mkv").unwrap();
        handle_fs_events(&fix.state, &tracker, vec![event(media.parent().unwrap())]);
        let store = fix.state.store();
        let store = store.lock();
        assert!(
            !store
                .missing_at_by_path()
                .unwrap()
                .contains_key(&row.path_key)
        );
        let restored = store
            .file_coverage_by_path(&row.file.path.to_string_lossy())
            .unwrap()
            .expect("restored STRM keeps its file row");
        assert_eq!(restored.file.file_id, row.file.file_id);
        assert_eq!(
            store.playback_progress(user, row.file.media_id).unwrap(),
            Some(12_345)
        );
    });
}

#[test]
fn directory_only_strm_url_change_with_same_size_and_mtime_is_processed() {
    let fix = PresenceFixture::new();
    runtime().block_on(async {
        let (media, tracker) = baseline(&fix).await;
        let before = operations(&fix).len();
        let mtime = std::fs::metadata(&media).unwrap().modified().unwrap();
        std::fs::write(&media, "https://example.invalid/other.mkv").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&media)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        handle_fs_events(&fix.state, &tracker, vec![event(media.parent().unwrap())]);
        assert_eq!(
            operations(&fix).len(),
            before + 1,
            "STRM content determines source identity"
        );
        let op = operations(&fix)
            .into_iter()
            .find(|op| op.status.is_active())
            .unwrap();
        assert_eq!(
            fix.wait_scan_terminal_op(&op.id).await.status,
            store::ScanOperationStatus::Completed
        );
    });
}

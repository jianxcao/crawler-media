#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use api::fs_watcher::{StrmGraceTracker, handle_fs_events};
use domain::{LibraryFile, UnitKey};
use notify_debouncer_mini::{DebouncedEvent, DebouncedEventKind};
use support::library_presence::PresenceFixture;

const FIRST_URL: &str = "https://example.invalid/first.mkv";
const SECOND_URL: &str = "https://example.invalid/other.mkv";

fn probed_file() -> (PresenceFixture, LibraryFile) {
    // Construct outside Tokio so no remote probe workers start.
    let fix = PresenceFixture::new();
    std::fs::write(fix.root.join("movie.strm"), FIRST_URL).unwrap();
    let (_, file) = fix.insert_media_and_ledger("movie.strm");
    let store = fix.state.store();
    store
        .put_file_meta_versioned(
            &file.file_id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack::default()),
                audio: Vec::new(),
                subtitles: Vec::new(),
            },
            Some(&file.version.source_version),
            Some(80_000),
        )
        .unwrap();
    store
        .put_cached_chapters(&file.file_id.to_string(), &[])
        .unwrap();
    let mut marker = store::test_support::fixture_marker(&store, file.media_id, UnitKey::WHOLE);
    marker.intro_start_ms = Some(1_000);
    marker.intro_end_ms = Some(5_000);
    marker.locked = true;
    store.put_fixture_marker(&marker).unwrap();
    (fix, file)
}

fn observe(fix: &PresenceFixture, file: &LibraryFile, tracker: &Arc<StrmGraceTracker>) {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        handle_fs_events(
            &fix.state,
            tracker,
            vec![DebouncedEvent::new(
                file.path.clone(),
                DebouncedEventKind::Any,
            )],
        );
    });
}

#[test]
fn first_watcher_observation_preserves_probed_unchanged_source() {
    let (fix, file) = probed_file();
    let tracker = Arc::new(StrmGraceTracker::new());
    observe(&fix, &file, &tracker);

    let store = fix.state.store();
    assert!(
        store
            .get_file_meta(&file.file_id.to_string())
            .unwrap()
            .is_some(),
        "first Watcher observation is not evidence of changed source"
    );
    assert!(
        store
            .get_cached_chapters(&file.file_id.to_string())
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_file_marker(file.file_id, UnitKey::WHOLE)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .file
            .version,
        file.version
    );
    assert!(store.scan_probe_obligations(10).unwrap().is_empty());
    assert!(
        store
            .scan_operations_for_library(&fix.library_id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn stale_watcher_memory_cannot_invalidate_the_current_recorded_source() {
    let (fix, file) = probed_file();
    let tracker = Arc::new(StrmGraceTracker::new());
    tracker.on_created_or_modified(&file.path, Some(SECOND_URL));
    observe(&fix, &file, &tracker);

    assert!(
        fix.state
            .store()
            .get_file_meta(&file.file_id.to_string())
            .unwrap()
            .is_some(),
        "SQLite source identity overrides stale Watcher memory"
    );
}

#[test]
fn changed_source_advances_version_and_durably_rearms_probe_even_when_memory_matches() {
    let (fix, file) = probed_file();
    std::fs::write(&file.path, SECOND_URL).unwrap();
    let tracker = Arc::new(StrmGraceTracker::new());
    // The Watcher can start after the file changed but before the next scan.
    tracker.on_created_or_modified(&file.path, Some(SECOND_URL));
    observe(&fix, &file, &tracker);

    let store = fix.state.store();
    let current = store
        .file_coverage_by_id(file.file_id)
        .unwrap()
        .unwrap()
        .file;
    assert_eq!(current.version.revision, file.version.revision + 1);
    assert_ne!(current.version.source_version, file.version.source_version);
    assert!(
        store
            .get_file_meta(&file.file_id.to_string())
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_cached_chapters(&file.file_id.to_string())
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_file_marker(file.file_id, UnitKey::WHOLE)
            .unwrap()
            .is_none(),
        "even manually locked markers cannot cross source versions"
    );
    assert!(
        store
            .scan_probe_obligations(10)
            .unwrap()
            .iter()
            .any(|obligation| obligation.ledger_id == file.file_id)
    );

    drop(store);
    observe(&fix, &current, &tracker);
    assert_eq!(
        fix.state
            .store()
            .file_coverage_by_id(file.file_id)
            .unwrap()
            .unwrap()
            .file
            .version,
        current.version
    );
}

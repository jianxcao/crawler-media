#[path = "support/mod.rs"]
mod support;

use notify_debouncer_mini::{DebouncedEvent, DebouncedEventKind};
use std::path::PathBuf;
use std::sync::Arc;
use support::library_presence::PresenceFixture;

fn event(path: PathBuf) -> DebouncedEvent {
    DebouncedEvent {
        path,
        kind: DebouncedEventKind::Any,
    }
}

#[tokio::test]
async fn directory_event_reconciles_all_pages_without_touching_sibling() {
    let fix = PresenceFixture::new();
    let mut rows = Vec::new();
    for n in 0..101 {
        rows.push(fix.insert_media_and_ledger(&format!("film/{n:03}.mkv")).1);
    }
    let sibling = fix.insert_media_and_ledger("film-extra/other.mkv").1;
    let tracker = Arc::new(api::fs_watcher::StrmGraceTracker::default());
    let directory = fix.root.join("film");

    api::fs_watcher::handle_fs_events(&fix.state, &tracker, vec![event(directory.clone())]);
    {
        let store = fix.state.store();
        let guard = store.as_ref();
        let missing = guard.missing_at_by_path().unwrap();
        for row in &rows {
            let key = guard.recorded_path_key(&row.path.to_string_lossy());
            assert!(missing.contains_key(&key), "missing {}", row.path.display());
        }
        let sibling_key = guard.recorded_path_key(&sibling.path.to_string_lossy());
        assert!(!missing.contains_key(&sibling_key));
    }

    {
        let mut files = fix.mock_files.files.lock();
        for row in &rows {
            files.insert(row.path.clone(), Ok(true));
        }
    }
    api::fs_watcher::handle_fs_events(&fix.state, &tracker, vec![event(directory)]);
    let store = fix.state.store();
    let guard = store.as_ref();
    let missing = guard.missing_at_by_path().unwrap();
    for row in &rows {
        let key = guard.recorded_path_key(&row.path.to_string_lossy());
        assert!(
            !missing.contains_key(&key),
            "not restored: {}",
            row.path.display()
        );
    }
}

#[tokio::test]
async fn last_local_file_missing_keeps_media_and_all_user_facts() {
    let fix = PresenceFixture::new();
    let (media_id, row) = fix.insert_media_and_ledger("movie.mkv");

    // Add user playback fact
    let user_id = domain::UserId::new();
    {
        let store_arc = fix.state.store();
        let guard = store_arc.as_ref();
        guard
            .set_playback_state(user_id, media_id, 5000, Some(true), Some(true), None, None)
            .unwrap();
    }

    // Trigger watcher missing event for the file
    let path = PathBuf::from(&row.path);
    let tracker = Arc::new(api::fs_watcher::StrmGraceTracker::default());
    api::fs_watcher::handle_fs_events(&fix.state, &tracker, vec![event(path.clone())]);

    // Ledger row still exists, but missing_at is set!
    let store_arc = fix.state.store();
    let guard = store_arc.as_ref();
    let loaded = guard.file_coverage_by_id(row.file_id).unwrap().unwrap();
    assert_eq!(loaded.file.file_id, row.file_id);
    assert!(
        guard
            .missing_at_by_path()
            .unwrap()
            .keys()
            .any(|key| key.ends_with(row.path.file_name().unwrap().to_str().unwrap()))
    );

    // Media and User facts PRESERVED (S1)
    assert!(guard.get_media(media_id).unwrap().is_some());
    assert_eq!(
        guard.playback_progress(user_id, media_id).unwrap(),
        Some(5000)
    );
}

#[tokio::test]
async fn last_strm_missing_after_45_seconds_keeps_ledger_and_history() {
    let fix = PresenceFixture::new();
    let (media_id, row) = fix.insert_media_and_ledger("sample.strm");

    let user_id = domain::UserId::new();
    {
        let store_arc = fix.state.store();
        let guard = store_arc.as_ref();
        guard
            .set_playback_state(user_id, media_id, 2500, Some(false), Some(true), None, None)
            .unwrap();
    }

    let path = PathBuf::from(&row.path);
    let tracker = Arc::new(api::fs_watcher::StrmGraceTracker::default());
    tracker.set_now(1000);

    // Event at t = 1000
    api::fs_watcher::handle_fs_events(&fix.state, &tracker, vec![event(path.clone())]);

    // Advance time past 45 seconds (t = 1050) and trigger handle_fs_events
    tracker.set_now(1050);
    api::fs_watcher::handle_fs_events(&fix.state, &tracker, vec![]);

    // Ledger row and user facts MUST STILL EXIST!
    let store_arc = fix.state.store();
    let guard = store_arc.as_ref();
    assert!(guard.file_coverage_by_id(row.file_id).unwrap().is_some());
    assert!(guard.get_media(media_id).unwrap().is_some());
    assert_eq!(
        guard.playback_progress(user_id, media_id).unwrap(),
        Some(2500)
    );
}

#[tokio::test]
async fn same_url_restore_clears_missing_and_preserves_probe_cache() {
    let fix = PresenceFixture::new();
    let (_media_id, row) = fix.insert_media_and_ledger("video.strm");

    // Populate the probe cache through the public writer: the fixture is a probed file.
    {
        let store_arc = fix.state.store();
        let guard = store_arc.as_ref();
        guard
            .put_file_meta_versioned(
                &row.file_id.to_string(),
                &library::Tracks {
                    video: Some(library::VideoTrack::default()),
                    audio: Vec::new(),
                    subtitles: Vec::new(),
                },
                None,
                Some(80_000),
            )
            .unwrap();
    }

    // Mark missing
    let path = PathBuf::from(&row.path);
    let tracker = Arc::new(api::fs_watcher::StrmGraceTracker::default());
    tracker.on_created_or_modified(&path, Some("http://example.com/video.m3u8"));
    api::fs_watcher::handle_fs_events(&fix.state, &tracker, vec![event(path.clone())]);

    let store_arc = fix.state.store();
    assert!(
        store_arc
            .as_ref()
            .missing_at_by_path()
            .unwrap()
            .keys()
            .any(|key| key.ends_with(row.path.file_name().unwrap().to_str().unwrap()))
    );

    // Write file back to disk with same content
    std::fs::write(&path, b"http://example.com/video.m3u8").unwrap();

    // Event present
    api::fs_watcher::handle_fs_events(&fix.state, &tracker, vec![event(path.clone())]);

    // missing_at is cleared
    assert!(
        !store_arc
            .as_ref()
            .missing_at_by_path()
            .unwrap()
            .keys()
            .any(|key| key.ends_with(row.path.file_name().unwrap().to_str().unwrap()))
    );

    // probe cache is preserved
    assert!(
        store_arc
            .as_ref()
            .get_file_meta(&row.file_id.to_string())
            .unwrap()
            .is_some()
    );
}

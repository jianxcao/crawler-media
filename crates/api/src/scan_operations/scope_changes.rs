use super::{Fixture, MediaKind, ScanOperationStatus};

/// Creating another kind cannot change strict ownership of the current scan.
#[tokio::test]
async fn another_kind_library_does_not_interrupt_running_scan() {
    let fix = Fixture::with_kind(MediaKind::Tv);
    let episode = fix.root.join("Example.S01E01.mkv");
    std::fs::write(&episode, b"fixture video").unwrap();
    let signature = fix.signature();
    let op = fix
        .state
        .store()
        .lock()
        .enqueue_scan(&fix.library_id, &signature, &[], 1000)
        .unwrap();

    // This hook runs after scope capture and before the first ledger publication.
    let state = fix.state.clone();
    let movie_root = fix.root.parent().unwrap().join("movies-new");
    std::fs::create_dir_all(&movie_root).unwrap();
    crate::http::library_scan::runner::test_observe_hook::set(
        &op.id,
        Box::new(move || {
            state
                .store()
                .lock()
                .create_library(
                    MediaKind::Movie,
                    "Other movies",
                    &[movie_root.to_str().unwrap()],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap();
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(
        done.status,
        ScanOperationStatus::Completed,
        "{}",
        done.report
    );
    let rows = fix.state.store().lock().list_file_coverage().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].file.path, episode);
}

#[tokio::test]
async fn disjoint_same_kind_library_does_not_interrupt_running_scan() {
    let fix = Fixture::new();
    let movie = fix.root.join("Example (2024).mkv");
    std::fs::write(&movie, b"fixture video").unwrap();
    let signature = fix.signature();
    let op = fix
        .state
        .store()
        .lock()
        .enqueue_scan(&fix.library_id, &signature, &[], 1000)
        .unwrap();
    let state = fix.state.clone();
    let other_root = fix.root.parent().unwrap().join("movies-other");
    std::fs::create_dir_all(&other_root).unwrap();
    crate::http::library_scan::runner::test_observe_hook::set(
        &op.id,
        Box::new(move || {
            state
                .store()
                .lock()
                .create_library(
                    MediaKind::Movie,
                    "Other movies",
                    &[other_root.to_str().unwrap()],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap();
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(
        done.status,
        ScanOperationStatus::Completed,
        "{}",
        done.report
    );
    let rows = fix.state.store().lock().list_file_coverage().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].file.path, movie);
}

#[tokio::test]
async fn disjoint_same_kind_root_update_does_not_interrupt_running_scan() {
    let fix = Fixture::new();
    let movie = fix.root.join("Example (2024).mkv");
    std::fs::write(&movie, b"fixture video").unwrap();
    let other_root = fix.root.parent().unwrap().join("other");
    let moved_root = fix.root.parent().unwrap().join("other-moved");
    std::fs::create_dir_all(&other_root).unwrap();
    std::fs::create_dir_all(&moved_root).unwrap();
    let other = fix
        .state
        .store()
        .lock()
        .create_library(
            MediaKind::Movie,
            "Other",
            &[other_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let signature = fix.signature();
    let op = fix
        .state
        .store()
        .lock()
        .enqueue_scan(&fix.library_id, &signature, &[], 1000)
        .unwrap();
    let state = fix.state.clone();
    crate::http::library_scan::runner::test_observe_hook::set(
        &op.id,
        Box::new(move || {
            state
                .store()
                .lock()
                .update_library(&other.id, None, Some(&[moved_root.to_str().unwrap()]), None)
                .unwrap();
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(
        done.status,
        ScanOperationStatus::Completed,
        "{}",
        done.report
    );
    assert!(
        fix.state
            .store()
            .lock()
            .file_coverage_by_path(&movie.display().to_string())
            .unwrap()
            .is_some()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_nested_library_prevents_stale_publication() {
    let fix = Fixture::new();
    let nested_root = fix.root.join("independent");
    std::fs::create_dir_all(&nested_root).unwrap();
    let movie = nested_root.join("Example (2024).mkv");
    std::fs::write(&movie, b"fixture video").unwrap();
    let alias = fix.root.parent().unwrap().join("outside-alias");
    std::os::unix::fs::symlink(&nested_root, &alias).unwrap();
    let signature = fix.signature();
    let op = fix
        .state
        .store()
        .lock()
        .enqueue_scan(&fix.library_id, &signature, &[], 1000)
        .unwrap();
    let state = fix.state.clone();
    crate::http::library_scan::runner::test_observe_hook::set(
        &op.id,
        Box::new(move || {
            state
                .store()
                .lock()
                .create_library(
                    MediaKind::Movie,
                    "Alias",
                    &[alias.to_str().unwrap()],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap();
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_ne!(
        done.status,
        ScanOperationStatus::Completed,
        "{}",
        done.report
    );
    assert!(
        fix.state
            .store()
            .lock()
            .list_file_coverage()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn frozen_library_prevents_scan_publication() {
    let fix = Fixture::new();
    let movie = fix.root.join("Example (2024).mkv");
    std::fs::write(&movie, b"fixture video").unwrap();
    let signature = fix.signature();
    let op = fix
        .state
        .store()
        .lock()
        .enqueue_scan(&fix.library_id, &signature, &[], 1000)
        .unwrap();
    let state = fix.state.clone();
    let library_id = fix.library_id.clone();
    crate::http::library_scan::runner::test_observe_hook::set(
        &op.id,
        Box::new(move || {
            state
                .store()
                .lock()
                .begin_library_removal(&library_id)
                .unwrap();
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_ne!(
        done.status,
        ScanOperationStatus::Completed,
        "{}",
        done.report
    );
    assert!(
        fix.state
            .store()
            .lock()
            .list_file_coverage()
            .unwrap()
            .is_empty()
    );
}

/// Same-kind ownership changes still invalidate the original publication scope.
#[tokio::test]
async fn nested_library_created_during_scan_prevents_stale_publication() {
    let fix = Fixture::new();
    let nested_root = fix.root.join("independent");
    std::fs::create_dir_all(&nested_root).unwrap();
    let movie = nested_root.join("Example (2024).mkv");
    std::fs::write(&movie, b"fixture video").unwrap();
    let signature = fix.signature();
    let op = fix
        .state
        .store()
        .lock()
        .enqueue_scan(&fix.library_id, &signature, &[], 1000)
        .unwrap();
    let state = fix.state.clone();
    crate::http::library_scan::runner::test_observe_hook::set(
        &op.id,
        Box::new(move || {
            state
                .store()
                .lock()
                .create_library(
                    MediaKind::Movie,
                    "Independent",
                    &[nested_root.to_str().unwrap()],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap();
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_ne!(
        done.status,
        ScanOperationStatus::Completed,
        "{}",
        done.report
    );
    assert!(
        fix.state
            .store()
            .lock()
            .list_file_coverage()
            .unwrap()
            .is_empty()
    );
}

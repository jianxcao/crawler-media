use crate::Store;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn checkpoint_window_has_a_bounded_database_work_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let stamps: Vec<_> = (0..5_000)
        .map(|i| (format!("/media/{i:05}.mkv"), 10, 5, None))
        .collect();
    store.record_scan_seen_stamps("operation", &stamps).unwrap();
    let steps = Arc::new(AtomicUsize::new(0));
    let count = steps.clone();
    // A one-file resume window must not read/materialize 5,000 prior checkpoints.
    // Abort runaway DB work, instead of asserting timing or a particular SQL shape.
    store.library.progress_handler(
        1_000,
        Some(move || count.fetch_add(1, Ordering::Relaxed) >= 20),
    );
    let requested = vec![stamps[2_500].0.clone()];
    let checkpoints = store
        .scan_seen_checkpoints("operation", &requested)
        .unwrap();
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(checkpoints[&requested[0]], (10, 5, None, false));
    assert!(steps.load(Ordering::Relaxed) < 20);
}

//! Playback session fixtures for API integration tests.
//!
//! Session issuance requires a published file whose revision and source version still
//! match the binding, so fixtures resolve the file instead of inventing a `LedgerId`.

use domain::{LedgerId, UnitKey, UserId};
use store::Store;

/// Issue a play session bound to an already-published fixture file.
pub fn issue_session(
    store: &Store,
    user: UserId,
    device: &str,
    file_id: LedgerId,
    now: i64,
) -> String {
    let file = store
        .published_files(&[file_id])
        .expect("read published file")
        .remove(&file_id)
        .unwrap_or_else(|| panic!("fixture file {file_id} is not published"));
    let unit = store
        .get_file_units(file_id)
        .expect("read file units")
        .first()
        .copied()
        .unwrap_or(UnitKey::WHOLE);
    store
        .issue_play_session(
            user,
            device,
            file_id,
            file.media_id,
            unit,
            &file.version,
            now,
        )
        .expect("issue fixture play session")
}

/// Monotonic client event numbers across fixture sessions.
pub fn next_sequence() -> i64 {
    static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

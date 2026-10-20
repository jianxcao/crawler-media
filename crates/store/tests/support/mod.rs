//! Shared fixtures for Store integration tests.
//!
//! Playback sessions are only issued for a published, version-matching file, so tests
//! that need a session must first seed one real file instead of inventing a `LedgerId`.

use domain::{Confidence, LedgerId, LedgerRow, MediaId, QualitySource, UnitKey, UserId};
use store::Store;

/// Seed one published whole-unit file under `root` and return its Media and file id.
///
/// A Library rooted at `root` is created on first use and reused afterwards, so a test
/// can seed several files without stacking Libraries.
pub fn seed_published_file(
    store: &Store,
    root: &std::path::Path,
    name: &str,
) -> (MediaId, LedgerId) {
    let media = domain::Media {
        id: MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: name.to_string(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: Vec::new(),
    };
    store.insert_media(&media).expect("insert fixture media");
    let ledger = publish_file(store, root, media.id, name);
    (media.id, ledger)
}

/// Publish one whole-unit file for an already-inserted Media under `root`.
///
/// A Library rooted at `root` is created on first use and reused afterwards, so a test
/// can publish several files without stacking Libraries.
pub fn publish_file(
    store: &Store,
    root: &std::path::Path,
    media_id: MediaId,
    name: &str,
) -> LedgerId {
    std::fs::create_dir_all(root).expect("fixture root");
    let root = std::fs::canonicalize(root).expect("canonical fixture root");
    let root_str = root.to_str().expect("utf-8 fixture root");
    let libraries = store.list_libraries().expect("list libraries");
    let owns_root = libraries.iter().any(|library| {
        library
            .root_paths
            .iter()
            .any(|candidate| candidate.to_string_lossy() == root_str)
    });
    if !owns_root {
        store
            .create_library(
                domain::MediaKind::Movie,
                "Fixture",
                &[root_str],
                "everyone",
                true,
                &[],
            )
            .expect("create fixture library");
    }

    let path = root.join(format!("{name}.mkv"));
    std::fs::write(&path, b"fixture").expect("write fixture file");
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: path.to_string_lossy().into_owned(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).expect("publish fixture file");
    row.id
}

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
        .issue_play_session(user, device, file_id, file.media_id, unit, &file.version, now)
        .expect("issue fixture play session")
}

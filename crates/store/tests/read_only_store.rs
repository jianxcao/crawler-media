//! L1: request reads run on a read-only `Store` facade that never takes the
//! shared writer lock. It must see committed writes and refuse to write.
use domain::{Media, MediaId, MediaKind};
use store::Store;

fn media(title: &str) -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

#[test]
fn reader_sees_committed_writes_and_refuses_to_write() {
    let tmp = tempfile::tempdir().unwrap();
    let writer = Store::open(tmp.path().join("data")).unwrap();
    let reader = writer.open_reader().unwrap();
    let first = media("First");
    writer.insert_media(&first).unwrap();
    assert_eq!(reader.get_media(first.id).unwrap().unwrap().title, "First");
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    writer
        .create_library(
            MediaKind::Movie,
            "M",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    assert_eq!(
        reader.list_library_roots().unwrap().len(),
        writer.list_library_roots().unwrap().len()
    );
    assert!(
        reader.insert_media(&media("Nope")).is_err(),
        "reader must be read-only"
    );
}

#[test]
fn reader_is_not_blocked_by_an_open_write_transaction() {
    let tmp = tempfile::tempdir().unwrap();
    let writer = Store::open(tmp.path().join("data")).unwrap();
    let reader = writer.open_reader().unwrap();
    let seen = media("Seen");
    writer.insert_media(&seen).unwrap();
    // Another writer holds library.db's write lock (as a long scan batch would).
    let blocker = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    assert_eq!(reader.get_media(seen.id).unwrap().unwrap().title, "Seen");
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    blocker.execute_batch("ROLLBACK").unwrap();
}

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::*;

static WAITING_FOR_WRITER: AtomicBool = AtomicBool::new(false);

fn busy(_: i32) -> bool {
    WAITING_FOR_WRITER.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(1));
    true
}

#[test]
fn competing_source_writer_prevents_old_fingerprint_publication() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let media_id = domain::MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: domain::MediaKind::Movie,
            title: "Source race".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let path = store
        .library_root(domain::MediaKind::Movie)
        .unwrap()
        .join("source.strm");
    std::fs::write(&path, "https://example.invalid/old.mkv\n").unwrap();
    let file = crate::test_support::publish_fixture_file(
        &store,
        &crate::test_support::FixtureFile {
            media_id,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let db_path = store
        .db_paths()
        .into_iter()
        .find(|path| path.ends_with("library.db"))
        .unwrap();
    let competing = rusqlite::Connection::open(db_path).unwrap();
    competing.execute_batch("BEGIN IMMEDIATE").unwrap();
    competing.execute(
        "UPDATE library_files SET file_revision = file_revision + 1, source_version = 'replacement' WHERE file_id = ?1",
        [file.file_id.to_string()],
    ).unwrap();
    store.library.busy_handler(Some(busy)).unwrap();
    WAITING_FOR_WRITER.store(false, Ordering::SeqCst);
    let writer = std::thread::spawn(move || {
        let result = store.put_fingerprint_cache_checked(
            &file.file_id.to_string(),
            Some(&file.version),
            &FingerprintCacheEntry {
                cache_key: "old-source-result".into(),
                algorithm_version: 1,
                sample_duration_secs: 180,
                media_duration_ms: Some(1000),
                intro: vec![1, 2, 3],
                outro: None,
            },
        );
        (
            result,
            store
                .get_fingerprint_cache(&file.file_id.to_string())
                .unwrap(),
        )
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    while !WAITING_FOR_WRITER.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::yield_now();
    }
    // Release the lock before asserting, so a failure cannot strand the worker.
    competing.execute_batch("COMMIT").unwrap();
    let (result, cache) = writer.join().unwrap();
    assert!(
        WAITING_FOR_WRITER.load(Ordering::SeqCst),
        "the actual writer must contend with the replacement"
    );
    assert!(
        result.is_err(),
        "a source committed by the competing writer must invalidate this capture"
    );
    assert!(
        cache.is_none(),
        "the old fingerprint must never land on the new FileVersion"
    );
}

//! TDD regressions for the 2026-11-21 storage-redesign merge review (P1-1, P1-3, P2-1, P2-2, P2-3, P2-10).
use std::path::PathBuf;

use domain::{
    Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, TypedItemId, UnitKey, UserId,
};
use rusqlite::Connection;
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{FilePresence, LedgerPresenceMode, LedgerPresenceObservation, ScannedLedger, Store};

fn open_fixture() -> (tempfile::TempDir, Store, PathBuf, PathBuf, MediaId) {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let store = Store::open(base.join("data")).unwrap();
    let a = base.join("a");
    let b = base.join("b");
    for (name, root) in [("A", &a), ("B", &b)] {
        std::fs::create_dir_all(root).unwrap();
        store
            .create_library(
                MediaKind::Movie,
                name,
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
    }
    let media = MediaId::new();
    store
        .insert_media(&Media {
            id: media,
            kind: MediaKind::Movie,
            title: "Arrival".into(),
            year: Some(2016),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    (tmp, store, a, b, media)
}

#[test]
fn p1_1_full_scan_purge_preserves_file_under_active_rename() {
    let (_tmp, store, a, b, media) = open_fixture();
    let from = a.join("Arrival (2016)").join("Arrival.mkv");
    std::fs::create_dir_all(from.parent().unwrap()).unwrap();
    std::fs::write(&from, b"arrival-video-bytes").unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: from.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let user = UserId::new();
    store
        .upsert_unit(
            user,
            media,
            -1,
            -1,
            42_000,
            Some(false),
            Some(true),
            Some(7_000_000),
            None,
            None,
            false,
            100,
        )
        .unwrap();

    // 1. Start organize/rename
    let to = b.join("Arrival (2016)").join("Arrival (2016).mkv");
    let req = store
        .begin_file_rename(file.file_id, &file.version, &from, &to, 200)
        .unwrap();

    // 2. Move bytes on disk (receipt not yet recorded)
    let receipt = store::library_write::execute_rename_disk(&req).unwrap();
    assert!(!from.exists());
    assert!(to.is_file());

    // 3a. Concurrent scan discovering `to` must not claim the active rename target
    let scan_insert = store.insert_scanned_files(
        &[ScannedLedger {
            file_id: LedgerId::new(),
            media_id: media,
            path: to.display().to_string(),
            units: vec![UnitKey::WHOLE],
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            source_version: "v-scan".into(),
            source_digest: None,
            size_bytes: Some(19),
            created_ns: None,
            modified_ns: None,
            artwork: Default::default(),
            observations: Vec::new(),
        }],
        201_000,
    );
    assert!(
        scan_insert.is_err(),
        "scan insert at active rename target must be refused"
    );

    // 3b. Concurrent full scan purge sees `from` missing on disk
    let coverage = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    let purge = store
        .apply_ledger_presence(
            &[LedgerPresenceObservation {
                file: coverage,
                path_key: from.display().to_string(),
                missing_at: None,
                presence: FilePresence::Missing,
                observed_source: None,
            }],
            LedgerPresenceMode::Purge,
            202,
        )
        .unwrap();
    assert_eq!(
        purge.deleted, 0,
        "active rename must protect file from purge"
    );
    assert_eq!(purge.skipped, 1);

    // 4. Record receipt & complete rename
    store.record_rename_disk(&receipt, 203).unwrap();
    store.complete_file_rename(&req.rename_id, 204).unwrap();

    // 5. Original file_id, favorite, 42s progress, and playable new path are all preserved
    let after = store
        .file_coverage_by_id(file.file_id)
        .unwrap()
        .expect("file_id preserved");
    assert_eq!(after.file.path, to);
    assert_eq!(after.file.presence, domain::FilePresence::Present);
    let watch = store.unit_state(user, media, -1, -1).unwrap().unwrap();
    assert!(watch.favorite);
    assert_eq!(watch.position_ms, 42_000);
    let resolved = store
        .resolve_playback(user, &TypedItemId::Work(media), None, None)
        .unwrap();
    assert_eq!(resolved.target.file.file_id, file.file_id);
}

#[test]
fn p1_3_rename_completion_keeps_sole_nfo_when_target_vanishes_or_changes() {
    for mutate_mode in ["delete", "replace", "intact"] {
        let (_tmp, store, a, b, media) = open_fixture();
        let src_dir = a.join("Arrival");
        let dst_dir = b.join("Arrival");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();
        let from = src_dir.join("Arrival.mkv");
        let from_nfo = src_dir.join("Arrival.nfo");
        let to = dst_dir.join("Arrival.mkv");
        let to_nfo = dst_dir.join("Arrival.nfo");
        std::fs::write(&from, b"video").unwrap();
        std::fs::write(&from_nfo, b"<movie><title>Original</title></movie>").unwrap();
        std::fs::write(&to_nfo, b"<movie><title>Original</title></movie>").unwrap();
        let file = publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media,
                path: from.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();

        let req = store
            .begin_file_rename(file.file_id, &file.version, &from, &to, 1)
            .unwrap();
        let receipt = store::library_write::execute_rename_disk(&req).unwrap();
        store.record_rename_disk(&receipt, 2).unwrap();

        match mutate_mode {
            "delete" => std::fs::remove_file(&to_nfo).unwrap(),
            "replace" => {
                std::fs::write(&to_nfo, b"<movie><title>Replaced</title></movie>").unwrap()
            }
            "intact" => {}
            _ => unreachable!(),
        }

        store.complete_file_rename(&req.rename_id, 3).unwrap();
        let queued = store.list_pending_file_deletions().unwrap();
        let nfo_queued = queued.iter().any(|p| p == &from_nfo.display().to_string());
        if mutate_mode == "intact" {
            assert!(nfo_queued, "intact identical target should queue old NFO");
        } else {
            assert!(
                !nfo_queued,
                "mode={mutate_mode}: must NOT queue sole remaining original NFO: {queued:?}"
            );
            assert_eq!(
                std::fs::read(&from_nfo).unwrap(),
                b"<movie><title>Original</title></movie>"
            );
        }
    }
}

#[test]
fn p2_1_same_device_recovery_rejects_in_place_same_length_strm_rewrite() {
    let (_tmp, store, a, b, media) = open_fixture();
    let from = a.join("Stream.strm");
    let to = b.join("Stream.strm");
    let url_a = b"https://cdn.example.com/media/aaaa1111";
    let url_b = b"https://cdn.example.com/media/bbbb2222";
    assert_eq!(url_a.len(), url_b.len());
    std::fs::write(&from, url_a).unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: from.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();

    let req = store
        .begin_file_rename(file.file_id, &file.version, &from, &to, 1)
        .unwrap();
    // Simulate same-device move completed before crash (receipt never recorded)
    std::fs::hard_link(&from, &to).unwrap();
    std::fs::remove_file(&from).unwrap();
    // Overwrite destination in-place with same byte length (preserves dev/ino/len)
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).open(&to).unwrap();
        f.write_all(url_b).unwrap();
        f.sync_all().unwrap();
    }

    let recovered = store::library_write::execute_rename_disk(&req);
    assert!(
        recovered.is_err(),
        "in-place same-length modified STRM must be rejected on recovery"
    );
    assert_eq!(std::fs::read(&to).unwrap(), url_b, "target bytes kept");
}

#[test]
fn p2_2_relinking_missing_file_restores_presence_and_playback_source() {
    let (_tmp, store, a, b, media) = open_fixture();
    let from = a.join("Lost.mkv");
    let to = b.join("Found.mkv");
    std::fs::write(&from, b"lost-and-found").unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: from.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();

    // Externally move file on disk, then mark missing via presence verify
    std::fs::rename(&from, &to).unwrap();
    let coverage = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    let verified = store
        .apply_ledger_presence(
            &[LedgerPresenceObservation {
                file: coverage,
                path_key: from.display().to_string(),
                missing_at: None,
                presence: FilePresence::Missing,
                observed_source: None,
            }],
            LedgerPresenceMode::Verify,
            10,
        )
        .unwrap();
    assert_eq!(verified.missing, 1);

    // Relink through reconciliation path
    let plan = store
        .plan_file_reconciliation(file.file_id, &file.version, &from, &to)
        .unwrap()
        .observe()
        .unwrap();
    let req = store.begin_observed_file_rename(plan, 20).unwrap();
    let _pub_guard = store.apply_file_rename(&req, 20).unwrap();

    // Must be restored to present, cleared from missing, and selectable for playback
    let after = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    assert_eq!(after.file.file_id, file.file_id);
    assert_eq!(after.file.path, to);
    assert_eq!(after.file.presence, domain::FilePresence::Present);
    let missing_rows = store
        .missing_files_page(after.file.library_id, None, 50)
        .unwrap();
    assert!(
        missing_rows.is_empty(),
        "relinked file must leave missing list: {missing_rows:?}"
    );
    let user = UserId::new();
    let resolved = store
        .resolve_playback(user, &TypedItemId::Work(media), None, None)
        .unwrap();
    assert_eq!(resolved.target.file.file_id, file.file_id);
}

#[test]
fn p2_3_failed_rename_on_occupied_target_releases_deletion_lock() {
    let (_tmp, store, a, b, media) = open_fixture();
    let from = a.join("Source.mkv");
    let to = b.join("Target.mkv");
    std::fs::write(&from, b"original-source").unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: from.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();

    let req = store
        .begin_file_rename(file.file_id, &file.version, &from, &to, 1)
        .unwrap();
    // Target gets occupied by another file on disk before disk phase runs
    std::fs::write(&to, b"foreign-occupant").unwrap();
    let err = match store.apply_file_rename(&req, 2) {
        Ok(_) => panic!("rename onto occupied target must fail"),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("occupied"),
        "unexpected error: {err}"
    );

    // Failed rename must no longer block deleting the untouched source file
    store
        .enqueue_file_deletion_observed(
            &from.display().to_string(),
            "user_delete",
            store::file_identity(&from.display().to_string()),
        )
        .expect("failed rename must release deletion lock");
    assert_eq!(std::fs::read(&to).unwrap(), b"foreign-occupant");
}

#[test]
fn p2_10_sidecar_sharing_query_plans_use_indexed_seeks() {
    let (tmp, _store, _a, _b, _media) = open_fixture();
    let conn = Connection::open(tmp.path().join("data").join("library.db")).unwrap();

    let mut plan_rows = Vec::new();
    let mut stmt = conn
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT EXISTS(SELECT 1 FROM library_files WHERE poster_path_key = ?2 AND file_id <> ?1)
                 OR EXISTS(SELECT 1 FROM library_files WHERE backdrop_path_key = ?2 AND file_id <> ?1)
                 OR EXISTS(SELECT 1 FROM library_files WHERE still_path_key = ?2 AND file_id <> ?1)
                 OR EXISTS(SELECT 1 FROM scrape_sidecars WHERE path_key = ?2 AND file_id <> ?1)",
        )
        .expect("indexed artwork & sidecar key columns must exist");
    let rows = stmt
        .query_map(["f1", "/a/Show/poster.jpg"], |r| r.get::<_, String>(3))
        .unwrap();
    for detail in rows {
        plan_rows.push(detail.unwrap());
    }
    let joined = plan_rows.join("\n");
    assert!(
        !joined.contains("SCAN library_files") && !joined.contains("SCAN scrape_sidecars"),
        "expected covering index seeks, got:\n{joined}"
    );

    let mut dir_stmt = conn
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT EXISTS(SELECT 1 FROM library_files WHERE parent_dir_key = ?2 AND file_id <> ?1)",
        )
        .expect("indexed parent_dir_key must exist");
    let dir_plan: Vec<String> = dir_stmt
        .query_map(["f1", "/a/Show"], |r| r.get::<_, String>(3))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let dir_joined = dir_plan.join("\n");
    assert!(
        !dir_joined.contains("SCAN library_files"),
        "expected parent_dir_key index seek, got:\n{dir_joined}"
    );
}

#[test]
fn p2_10_sidecar_sharing_work_stays_bounded_at_100k_files() {
    fn measure_rename_sql_steps(unrelated_files: i64) -> (i64, std::time::Duration) {
        let (tmp, store, a, b, media) = open_fixture();
        let libs = store.list_libraries().unwrap();
        let lib_a = libs.iter().find(|l| l.name == "A").unwrap();
        let root_a = store
            .list_library_roots()
            .unwrap()
            .into_iter()
            .find(|r| r.library_id == lib_a.id)
            .unwrap();
        // Bulk-insert `unrelated_files` under root A (including both nested subdirs and artwork/sidecars)
        {
            let conn = Connection::open(tmp.path().join("data").join("library.db")).unwrap();
            conn.execute_batch("PRAGMA synchronous = OFF;").unwrap();
            let tx = conn.unchecked_transaction().unwrap();
            tx.execute(
                "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < ?1)
                 INSERT INTO library_files(
                    file_id, library_id, root_id, media_id, kind, path_key, display_path,
                    presence_state, publication_state, file_revision, source_version,
                    quality_source, confidence,
                    poster_path, poster_path_key, has_poster
                 )
                 SELECT
                    printf('%08x-0000-4000-8000-%012x', x, x),
                    ?2, ?3, ?4, 'movie',
                    ?5 || printf('/Bulk/Sub%d/Movie%d.mkv', x % 100, x),
                    ?5 || printf('/Bulk/Sub%d/Movie%d.mkv', x % 100, x),
                    'present', 'published', 1, 'v1', 'release', 'high',
                    ?5 || printf('/Bulk/Sub%d/poster-%d.jpg', x % 100, x),
                    ?5 || printf('/Bulk/Sub%d/poster-%d.jpg', x % 100, x),
                    1
                 FROM n",
                rusqlite::params![
                    unrelated_files,
                    lib_a.id.to_string(),
                    root_a.id,
                    media.to_string(),
                    a.display().to_string(),
                ],
            )
            .unwrap();
            tx.execute(
                "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < ?1)
                 INSERT INTO scrape_sidecars(file_id, kind, path, path_key)
                 SELECT
                    printf('%08x-0000-4000-8000-%012x', x, x),
                    'nfo',
                    ?2 || printf('/Bulk/Sub%d/Movie%d.nfo', x % 100, x),
                    ?2 || printf('/Bulk/Sub%d/Movie%d.nfo', x % 100, x)
                 FROM n",
                rusqlite::params![unrelated_files, a.display().to_string()],
            )
            .unwrap();
            tx.commit().unwrap();
        }

        let src_dir = a.join("Target");
        std::fs::create_dir_all(&src_dir).unwrap();
        let from = src_dir.join("Target.mkv");
        std::fs::write(&from, b"video").unwrap();
        std::fs::write(src_dir.join("poster.jpg"), b"poster").unwrap();
        std::fs::write(src_dir.join("fanart.jpg"), b"fanart").unwrap();
        std::fs::write(src_dir.join("Target.nfo"), b"<movie/>").unwrap();
        let file = publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media,
                path: from.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();

        let to = b.join("Target").join("Target.mkv");
        let plan = store
            .plan_file_rename(file.file_id, &file.version, &from, &to)
            .unwrap()
            .observe()
            .unwrap();

        let started = std::time::Instant::now();
        let req = store.begin_observed_file_rename(plan, 1).unwrap();
        let elapsed = started.elapsed();

        let conn = Connection::open(tmp.path().join("data").join("library.db")).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT EXISTS(SELECT 1 FROM library_files WHERE poster_path_key = ?2 AND file_id <> ?1)
                     OR EXISTS(SELECT 1 FROM library_files WHERE backdrop_path_key = ?2 AND file_id <> ?1)
                     OR EXISTS(SELECT 1 FROM library_files WHERE still_path_key = ?2 AND file_id <> ?1)
                     OR EXISTS(SELECT 1 FROM scrape_sidecars WHERE path_key = ?2 AND file_id <> ?1)",
            )
            .unwrap();
        let _: bool = stmt
            .query_row(
                rusqlite::params![
                    req.file_id.to_string(),
                    src_dir.join("poster.jpg").display().to_string()
                ],
                |r| r.get(0),
            )
            .unwrap();
        let vm_steps = i64::from(stmt.get_status(rusqlite::StatementStatus::VmStep));
        (vm_steps, elapsed)
    }

    let (steps_1k, dur_1k) = measure_rename_sql_steps(1_000);
    let (steps_100k, dur_100k) = measure_rename_sql_steps(100_000);
    assert!(
        steps_100k <= steps_1k * 3 + 20,
        "indexed sidecar lookup steps must stay O(log N): 1k={steps_1k} ({dur_1k:?}), 100k={steps_100k} ({dur_100k:?})"
    );
    assert!(
        dur_100k < std::time::Duration::from_secs(2),
        "writer lock during begin_observed_file_rename at 100k rows must be fast: {dur_100k:?}"
    );
}

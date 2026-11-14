mod query_model_support;
use query_model_support::*;
#[test]
fn last_file_removal_removes_counter_members() {
    let (tmp, store, lib, _) = setup();
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let files: Vec<domain::LedgerId> = db
        .prepare("SELECT file_id FROM library_files WHERE library_id=?1")
        .unwrap()
        .query_map([lib.to_string()], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap().parse().unwrap())
        .collect();
    store.delete_files(lib, &files).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_query_counts WHERE library_id=?1",
            [lib.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn unchanged_members_and_playback_do_not_rewrite_static_counts() {
    let (tmp, store, _, ids) = setup();
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_count_insert BEFORE INSERT ON library_query_counts BEGIN SELECT RAISE(ABORT, 'unexpected counter insert'); END;
        CREATE TRIGGER reject_count_update BEFORE UPDATE ON library_query_counts BEGIN SELECT RAISE(ABORT, 'unexpected counter update'); END;
        CREATE TRIGGER reject_count_delete BEFORE DELETE ON library_query_counts BEGIN SELECT RAISE(ABORT, 'unexpected counter delete'); END;").unwrap();
    let media = ids[0].parse().unwrap();
    let (fields, revision) = store
        .get_metadata_overrides(media, domain::EntityScope::Work, "en-US")
        .unwrap()
        .unwrap();
    store
        .set_metadata_overrides(media, domain::EntityScope::Work, "en-US", &fields, revision)
        .unwrap();
    store
        .mutate_playback(
            media,
            &store::playback::PlaybackMutation::marks(
                domain::UserId::new(),
                -1,
                -1,
                Some(true),
                Some(true),
            ),
            10,
        )
        .unwrap();
}

#[test]
fn one_hundred_file_window_settles_one_card() {
    let (tmp, store, lib, ids) = setup_count(1);
    let media = ids[0].parse().unwrap();
    let template = store.file_coverage_for_media(media).unwrap()[0]
        .file
        .clone();
    let files = (0..100)
        .map(|n| {
            let mut file = template.clone();
            file.file_id = domain::LedgerId::new();
            file.path = format!("/movies/extra-{n}.mkv").into();
            file
        })
        .collect::<Vec<_>>();
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let settlements = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let observed = settlements.clone();
    store.with_writer(store::Db::Library, |conn| {
        conn.create_scalar_function(
            "measure_projection_settlement", 0, rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            move |_| {
                observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(0_i64)
            },
        )?;
        conn.execute_batch("CREATE TRIGGER count_marks AFTER UPDATE OF query_projection_revision ON library_items BEGIN SELECT measure_projection_settlement(); END;")?;
        Ok(())
    }).unwrap();
    let publications = files
        .iter()
        .map(|file| store::library_write::FilePublication {
            file,
            units: &[domain::UnitKey::WHOLE],
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        })
        .collect::<Vec<_>>();
    store
        .publish_files(
            &store::library_write::WriteScope {
                library_id: lib,
                config_revision: 0,
            },
            &publications,
            1,
        )
        .unwrap();
    let (revision, projection): (i64, i64) = db.query_row(
        "SELECT item_revision,query_projection_revision FROM library_items WHERE library_id=?1 AND media_id=?2",
        rusqlite::params![lib.to_string(), media.to_string()], |r| Ok((r.get(0)?, r.get(1)?)),
    ).unwrap();
    assert_eq!(
        settlements.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the whole window settles one query projection"
    );
    assert_eq!(projection, revision);
    assert_eq!(db.query_row("SELECT member_count FROM library_query_counts WHERE surface='browse' AND dimension='all'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(
        store
            .get_library_item(lib, media)
            .unwrap()
            .unwrap()
            .file_count,
        101
    );
}

#[test]
fn failed_settlement_rolls_back_file_facts_members_counts_and_revision() {
    let (tmp, store, lib, ids) = setup_count(1);
    let media = ids[0].parse().unwrap();
    let mut file = store.file_coverage_for_media(media).unwrap()[0]
        .file
        .clone();
    file.file_id = domain::LedgerId::new();
    file.path = "/movies/fault.mkv".into();
    file.resolution = Some("2160p".into());
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let before = store.get_library_item(lib, media).unwrap();
    db.execute_batch("CREATE TRIGGER reject_settle BEFORE INSERT ON library_query_counts WHEN NEW.value='2160p' BEGIN SELECT RAISE(ABORT, 'settle fault'); END;").unwrap();
    assert!(
        store
            .publish_files(
                &store::library_write::WriteScope {
                    library_id: lib,
                    config_revision: 0
                },
                &[store::library_write::FilePublication {
                    file: &file,
                    units: &[domain::UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false
                }],
                1
            )
            .is_err()
    );
    assert_eq!(store.get_library_item(lib, media).unwrap(), before);
    assert_eq!(store.file_coverage_for_media(media).unwrap().len(), 1);
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM library_item_terms WHERE term_value='2160p'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    run(&store, lib, false, "after-fault");
    assert!(
        !store
            .library_integrity_report("after-fault")
            .unwrap()
            .has_issues()
    );
}

#[test]
fn last_file_merge_and_cross_library_moves_conserve_counts() {
    let (tmp, store, lib, ids) = setup_count(2);
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let revision = |id: &str| {
        db.query_row(
            "SELECT metadata_revision FROM media WHERE media_id=?1",
            [id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    };
    let request = store::playback::MediaMergeRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        survivor: ids[0].parse().unwrap(),
        loser: ids[1].parse().unwrap(),
        survivor_revision: revision(&ids[0]),
        loser_revision: revision(&ids[1]),
        refresh_targets: std::collections::BTreeMap::from([("tmdb".into(), ids[0].clone())]),
    };
    let op = store.begin_media_merge(&request, 0).unwrap();
    assert_eq!(
        store.advance_media_merge(&op, 1).unwrap().state,
        "succeeded"
    );
    assert_eq!(browse_total(&db, lib), 1);
    let files = store.file_coverage_for_media(request.survivor).unwrap();
    store
        .delete_files(
            lib,
            &files.iter().map(|row| row.file.file_id).collect::<Vec<_>>(),
        )
        .unwrap();
    assert_eq!(browse_total(&db, lib), 0);
    let mut file = files[0].file.clone();
    file.file_id = domain::LedgerId::new();
    file.path = "/movies/nested/move.mkv".into();
    store
        .publish_files(
            &store::library_write::WriteScope {
                library_id: lib,
                config_revision: 0,
            },
            &[store::library_write::FilePublication {
                file: &file,
                units: &[domain::UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            2,
        )
        .unwrap();
    let target = store
        .create_library(
            domain::MediaKind::Movie,
            "Nested",
            &["/movies/nested"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store.drain_root_reassignments().unwrap();
    assert_eq!(browse_total(&db, lib), 0);
    assert_eq!(browse_total(&db, target.id.parse().unwrap()), 1);
}

fn browse_total(db: &rusqlite::Connection, lib: domain::LibraryId) -> i64 {
    db.query_row("SELECT COALESCE(SUM(member_count),0) FROM library_query_counts WHERE library_id=?1 AND surface='browse' AND dimension='all'", [lib.to_string()], |r| r.get(0)).unwrap()
}

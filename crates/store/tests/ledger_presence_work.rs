//! Presence windows stay bounded and settle each changed Media once.
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{FilePresence, LedgerPresenceMode, Store};

fn media(store: &Store) -> MediaId {
    let id = MediaId::new();
    store
        .insert_media(&Media {
            id,
            kind: MediaKind::Movie,
            title: "Presence work".into(),
            original_title: None,
            aliases: vec![],
            year: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    id
}

fn file(store: &Store, media_id: MediaId, path: String) {
    publish_fixture_file(
        store,
        &FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id,
            path,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn presence_page_limits_files_without_cutting_coverage_and_settles_media_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Window",
            &["/presence-window"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let id = media(&store);
    for n in 0..4 {
        file(&store, id, format!("/presence-window/{n}.mkv"));
    }
    let library_id = library.id.parse().unwrap();
    let mut all = vec![];
    let mut after = None;
    loop {
        let page = store
            .ledger_presence_page(library_id, Some(id), after, 2)
            .unwrap();
        if page.is_empty() {
            break;
        }
        assert!(page.len() <= 2);
        assert!(page.iter().all(|obs| obs.file.units.len() == 1));
        after = Some(page.last().unwrap().file.file.file_id);
        all.extend(page);
    }
    assert_eq!(all.len(), 4);
    let db = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    db.execute_batch("CREATE TABLE presence_settlements(n INTEGER); INSERT INTO presence_settlements VALUES(0);
        CREATE TRIGGER count_presence_settle AFTER UPDATE OF query_projection_revision ON library_items
        BEGIN UPDATE presence_settlements SET n=n+1; END;").unwrap();
    for obs in &mut all {
        obs.presence = FilePresence::Missing;
    }
    let result = store
        .apply_ledger_presence(&all, LedgerPresenceMode::Verify, 10)
        .unwrap();
    assert_eq!(result.missing, 4);
    assert_eq!(result.applied.len(), 4);
    let count: i64 = db
        .query_row("SELECT n FROM presence_settlements", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "one projection settlement per affected Media");
    let item = store.get_library_item(library_id, id).unwrap().unwrap();
    assert_eq!(
        (
            item.file_count,
            item.available_file_count,
            item.missing_file_count
        ),
        (4, 0, 4)
    );
    let user = domain::UserId::new();
    store
        .insert_user(&domain::User {
            id: user,
            login: "presence-user".into(),
            enabled: true,
            role: domain::UserRole::Member,
        })
        .unwrap();
    store
        .set_playback_state(user, id, 100, None, Some(true), None, None)
        .unwrap();
    assert_eq!(store.playback_marks(user, id).unwrap(), Some((false, true)));
    let missing = store
        .ledger_presence_page(library_id, Some(id), None, 100)
        .unwrap();
    let pruned = store
        .apply_ledger_presence(&missing, LedgerPresenceMode::Prune, 20)
        .unwrap();
    assert_eq!(pruned.deleted, 4);
    assert!(store.get_library_item(library_id, id).unwrap().is_none());
    assert!(store.file_coverage_for_media(id).unwrap().is_empty());
    assert_eq!(store.playback_marks(user, id).unwrap(), Some((false, true)));
    assert!(store.get_media(id).unwrap().is_some());
    assert!(
        store
            .ledger_presence_page(library_id, None, None, 101)
            .is_err()
    );
}

#[test]
fn unrelated_library_growth_does_not_increase_presence_window_sql_work() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let target = store
        .create_library(
            MediaKind::Movie,
            "Target",
            &["/presence-target"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let other = store
        .create_library(
            MediaKind::Movie,
            "Other",
            &["/presence-other"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let id = media(&store);
    for n in 0..4 {
        file(&store, id, format!("/presence-target/{n}.mkv"));
    }
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id == other.id)
        .unwrap()
        .id;
    let mut db = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    let mut previous = 0;
    let mut baseline = None;
    for count in [100, 1000, 10_000] {
        let tx = db.transaction().unwrap();
        for n in previous..count {
            tx.execute("INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,
                presence_state,publication_state,file_revision,source_version,quality_source,confidence)
                VALUES(?1,?2,?3,?4,'movie',?5,?5,'present','published',1,'v1','release','high')",
                rusqlite::params![LedgerId::new().to_string(), other.id, root, id.to_string(), format!("/presence-other/{n}.mkv")]).unwrap();
        }
        tx.commit().unwrap();
        previous = count;
        if count == 10_000 {
            for filter in ["", "AND media_id=?2"] {
                let plan = format!(
                    "EXPLAIN QUERY PLAN SELECT * FROM library_files WHERE library_id=?1 {filter} AND file_id>'' ORDER BY file_id LIMIT 100"
                );
                let mut stmt = db.prepare(&plan).unwrap();
                let values = if filter.is_empty() {
                    vec![target.id.clone()]
                } else {
                    vec![target.id.clone(), id.to_string()]
                };
                let details = stmt
                    .query_map(rusqlite::params_from_iter(values), |r| {
                        r.get::<_, String>(3)
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                println!("presence_page_plan={details:?}");
            }
        }
        let (rows, vm, scans) = store
            .ledger_presence_page_with_work(target.id.parse().unwrap(), Some(id), None, 100)
            .unwrap();
        assert_eq!(rows.len(), 4);
        let (unfiltered, all_vm, all_scans) = store
            .ledger_presence_page_with_work(target.id.parse().unwrap(), None, None, 100)
            .unwrap();
        assert_eq!(unfiltered.len(), 4);
        println!(
            "unrelated_files={count} scoped_vm={vm} scoped_fullscan={scans} library_vm={all_vm} library_fullscan={all_scans}"
        );
        let work = (vm, scans, all_vm, all_scans);
        if let Some(expected) = baseline {
            assert_eq!(work, expected);
        } else {
            baseline = Some(work);
        }
    }
}

#[test]
fn one_file_page_retains_every_unit_of_merged_coverage() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            "Merged",
            &["/presence-merged"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let id = MediaId::new();
    store
        .insert_media(&Media {
            id,
            kind: MediaKind::Tv,
            title: "Merged coverage".into(),
            original_title: None,
            aliases: vec![],
            year: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let units = [
        domain::UnitKey::new(1, 1),
        domain::UnitKey::new(1, 2),
        domain::UnitKey::new(1, 3),
    ];
    store::test_support::publish_fixture_file_units(
        &store,
        &FixtureFile {
            media_id: id,
            path: "/presence-merged/merged.mkv".into(),
            ..Default::default()
        },
        &units,
    )
    .unwrap();
    let page = store
        .ledger_presence_page(library.id.parse().unwrap(), Some(id), None, 1)
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].file.units, units);
}

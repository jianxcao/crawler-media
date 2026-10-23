//! N04: user summaries are scoped to the seasons/episodes a Library owns, so a
//! Library without any of the user's units legally has no user row.
use domain::{LibraryId, Media, MediaId, MediaKind, UserId};
use rusqlite::{Connection, params};
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

struct Fx {
    tmp: tempfile::TempDir,
    store: Store,
    a: LibraryId,
    b: LibraryId,
    media: MediaId,
    user: UserId,
}

fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let mut libs = Vec::new();
    for name in ["a", "b"] {
        let root = tmp.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        let lib = store
            .create_library(
                MediaKind::Tv,
                name,
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        libs.push(lib.id.parse::<LibraryId>().unwrap());
    }
    let media = MediaId::new();
    store
        .insert_media(&Media {
            id: media,
            kind: MediaKind::Tv,
            title: "Show".into(),
            year: Some(2020),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    for (dir, season) in [("a", 1), ("b", 2)] {
        let path = tmp.path().join(dir).join(format!("Show/S0{season}E01.mkv"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"x").unwrap();
        publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                season: Some(season),
                episode: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let user = UserId::new();
    store
        .mutate_playback(
            media,
            &store::playback::PlaybackMutation::marks(user, 2, 1, None, Some(true)),
            10,
        )
        .unwrap();
    Fx {
        tmp,
        store,
        a: libs[0],
        b: libs[1],
        media,
        user,
    }
}

fn run(
    store: &Store,
    library: LibraryId,
    repair: bool,
    op: &str,
) -> store::library_operations::integrity::IntegrityReport {
    store
        .begin_library_integrity(library, repair, op, 2000)
        .unwrap();
    for step in 0..50 {
        let status = store.advance_library_operation(op, 2001 + step, 1).unwrap();
        if status.state != "applying" {
            return store.library_integrity_report(op).unwrap();
        }
    }
    panic!("integrity did not finish");
}

#[test]
fn library_without_users_units_is_not_reported_missing() {
    let fx = fixture();
    let report = run(&fx.store, fx.a, false, "check-a");
    assert!(!report.has_issues(), "{report:?}");
    let report = run(&fx.store, fx.b, false, "check-b");
    assert!(!report.has_issues(), "{report:?}");
}

#[test]
fn really_missing_user_row_is_found_and_repaired_stably() {
    let fx = fixture();
    let db = Connection::open(fx.tmp.path().join("data/library.db")).unwrap();
    let removed = db
        .execute(
            "DELETE FROM user_library_items WHERE library_id = ?1 AND media_id = ?2 AND user_id = ?3",
            params![fx.b.to_string(), fx.media.to_string(), fx.user.to_string()],
        )
        .unwrap();
    assert_eq!(removed, 1);
    let report = run(&fx.store, fx.b, false, "check");
    assert_eq!(
        report.missing_user_items,
        vec![(fx.user, fx.media)],
        "{report:?}"
    );
    run(&fx.store, fx.b, true, "repair");
    for (lib, op) in [(fx.a, "after-a"), (fx.b, "after-b")] {
        let report = run(&fx.store, lib, false, op);
        assert!(!report.has_issues(), "{report:?}");
    }
    let rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM user_library_items WHERE media_id = ?1",
            params![fx.media.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1, "repair must not fabricate a row for library A");
}

/// R07: a derived summary whose raw playback facts were removed out of band is an orphan;
/// check reports it, repair deletes it, and the recheck is clean.
#[test]
fn orphan_summary_without_raw_user_is_repaired() {
    let fx = fixture();
    let db = Connection::open(fx.tmp.path().join("data/library.db")).unwrap();
    let removed = db
        .execute(
            "DELETE FROM playback_units WHERE user_id = ?1 AND media_id = ?2",
            params![fx.user.to_string(), fx.media.to_string()],
        )
        .unwrap();
    assert_eq!(removed, 1);
    let unrelated = |db: &Connection| -> i64 {
        db.query_row(
            "SELECT COUNT(*) FROM user_library_items WHERE library_id = ?1",
            params![fx.a.to_string()],
            |r| r.get(0),
        )
        .unwrap()
    };
    let unrelated_before = unrelated(&db);
    let before = run(&fx.store, fx.b, false, "check");
    assert!(
        before.has_issues(),
        "orphan summary must be reported: {before:?}"
    );
    run(&fx.store, fx.b, true, "repair");
    let after_summary: Option<i64> = db
        .query_row(
            "SELECT summary_revision FROM user_library_items
             WHERE library_id = ?1 AND media_id = ?2 AND user_id = ?3",
            params![fx.b.to_string(), fx.media.to_string(), fx.user.to_string()],
            |r| r.get(0),
        )
        .ok();
    assert!(after_summary.is_none(), "repair deletes the orphan summary");
    let recheck = run(&fx.store, fx.b, false, "recheck");
    assert!(!recheck.has_issues(), "{recheck:?}");
    assert_eq!(unrelated(&db), unrelated_before);
}

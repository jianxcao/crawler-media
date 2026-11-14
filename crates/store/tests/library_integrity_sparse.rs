//! Sparse continuation state belongs to touched user/Media pairs across all real
//! owning Libraries; browse fields still describe only each Library's own units.
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
    for (dir, season) in [("a", 3), ("b", 2)] {
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
fn all_owning_libraries_have_clean_sparse_user_continuation() {
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
    for library in [fx.a, fx.b] {
        let row = fx
            .store
            .get_user_library_summary(fx.user, library, fx.media)
            .unwrap()
            .unwrap();
        assert_eq!(row.favorite, library == fx.b, "browse favorite stays local");
        assert!(!row.seen && !row.played);
        assert_eq!(row.last_played_at, 0);
    }
    let untouched_user = UserId::new();
    for library in [fx.a, fx.b] {
        assert!(
            fx.store
                .get_user_library_summary(untouched_user, library, fx.media)
                .unwrap()
                .is_none()
        );
    }
    let unrelated = publish_untouched_media(&fx);
    assert!(
        fx.store
            .get_user_library_summary(fx.user, fx.a, unrelated)
            .unwrap()
            .is_none()
    );
    assert_global_continuation_uses_other_season_owner(&fx);
}

fn publish_untouched_media(fx: &Fx) -> MediaId {
    let mut media = fx.store.get_media(fx.media).unwrap().unwrap();
    media.id = MediaId::new();
    media.title = "Untouched show".into();
    fx.store.insert_media(&media).unwrap();
    let path = fx.tmp.path().join("a/Untouched/S01E01.mkv");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"unstarted").unwrap();
    publish_fixture_file(
        &fx.store,
        &FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    media.id
}

fn assert_global_continuation_uses_other_season_owner(fx: &Fx) {
    fx.store
        .upsert_unit(
            fx.user,
            fx.media,
            2,
            1,
            30_000,
            None,
            None,
            Some(600_000),
            None,
            None,
            false,
            10,
        )
        .unwrap();
    fx.store
        .mutate_playback(
            fx.media,
            &store::playback::PlaybackMutation::marks(fx.user, 2, 1, Some(true), None),
            20_000,
        )
        .unwrap();
    let next = fx
        .store
        .next_up_page(&store::ContinueQuery {
            user_id: fx.user,
            libraries: vec![fx.a, fx.b],
            parent: domain::ProtocolParent::Root,
            media_id: Some(fx.media),
            season: None,
            limit: 20,
            offset: 0,
        })
        .unwrap();
    assert_eq!(next.total, 1);
    assert_eq!(next.items[0].reference.library_id, fx.a);
    assert_eq!(
        next.items[0].reference.item,
        domain::TypedItemId::Unit {
            media: fx.media,
            key: domain::UnitKey::new(3, 1),
        }
    );
    assert_eq!(next.items[0].last_played_at, 10);
    let home = fx
        .store
        .home_activity_page(&store::HomeQuery {
            user_id: fx.user,
            libraries: vec![fx.a, fx.b],
            limit: 20,
        })
        .unwrap();
    assert_eq!(
        home.len(),
        1,
        "future owning Library needs global continuation even without local viewing facts"
    );
    assert_eq!(home[0].reference, next.items[0].reference);
    assert_eq!(home[0].last_activity, 10);
    assert!(home[0].advanced);
    let future = fx
        .store
        .get_user_library_summary(fx.user, fx.a, fx.media)
        .unwrap()
        .unwrap();
    assert!(!future.seen && !future.played && !future.favorite);
    assert_eq!(
        future.last_played_at, 0,
        "global continuation must not become local viewing activity"
    );
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

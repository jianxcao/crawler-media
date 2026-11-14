//! Continuation projections follow raw facts through repair and ownership changes.
use domain::{
    LibraryId, Media, MediaId, MediaKind, ProtocolParent, TypedItemId, UnitKey, UnitScope, UserId,
};
use std::collections::BTreeMap;
use store::library_operations::integrity::IntegrityReport;
use store::playback::{MediaMergeRequest, PlaybackMutation};
use store::test_support::{FixtureFile, publish_fixture_file_units};
use store::{ContinueQuery, HomeQuery, Store};

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    user: UserId,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        Self {
            dir,
            store,
            user: UserId::new(),
        }
    }
    fn library(&self, name: &str) -> LibraryId {
        let root = self.dir.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        self.store
            .create_library(
                MediaKind::Tv,
                name,
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
            .parse()
            .unwrap()
    }
    fn media(&self) -> MediaId {
        let id = MediaId::new();
        self.store
            .insert_media(&Media {
                id,
                kind: MediaKind::Tv,
                title: id.to_string(),
                year: None,
                original_title: None,
                tmdb_id: Some(id.to_string()),
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
                aliases: vec![],
            })
            .unwrap();
        id
    }
    fn own(&self, library: LibraryId, media: MediaId, episode: i32) -> domain::LibraryFile {
        let root = self
            .store
            .get_library(&library.to_string())
            .unwrap()
            .unwrap()
            .root_paths[0]
            .clone();
        let path = root.join(format!("{media}-E{episode}.mkv"));
        std::fs::write(&path, b"video").unwrap();
        publish_fixture_file_units(
            &self.store,
            &FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                ..Default::default()
            },
            &[UnitKey::new(1, episode)],
        )
        .unwrap()
    }
    fn progress(&self, media: MediaId, episode: i32, clock: i64) {
        self.store
            .upsert_unit(
                self.user,
                media,
                1,
                episode,
                30_000,
                None,
                None,
                Some(600_000),
                None,
                None,
                false,
                clock,
            )
            .unwrap();
    }
    fn query(&self, libraries: &[LibraryId]) -> ContinueQuery {
        ContinueQuery {
            user_id: self.user,
            libraries: libraries.to_vec(),
            parent: ProtocolParent::Root,
            media_id: None,
            season: None,
            limit: 20,
            offset: 0,
        }
    }
    fn integrity(&self, library: LibraryId, repair: bool) -> IntegrityReport {
        let operation = uuid::Uuid::new_v4().to_string();
        self.store
            .begin_library_integrity(library, repair, &operation, 10_000)
            .unwrap();
        for step in 1..100 {
            let status = self
                .store
                .advance_library_operation(&operation, 10_000 + step, 1)
                .unwrap();
            assert_ne!(status.state, "failed", "{status:?}");
            if status.state == "succeeded" {
                return self.store.library_integrity_report(&operation).unwrap();
            }
        }
        panic!("finite integrity operation did not finish");
    }
}
fn episode(reference: &domain::ProtocolItemRef) -> (MediaId, i32) {
    match reference.item {
        TypedItemId::Unit { media, key } => (media, key.episode),
        _ => panic!("TV continuation must select an episode"),
    }
}

#[test]
fn integrity_independently_repairs_default_anchor_resume_clock_and_sparse_units() {
    for damage in [
        "UPDATE library_items SET first_owned_season=99,first_owned_episode=99 WHERE media_id=?1",
        "UPDATE user_library_items SET activity_season=99,activity_episode=99 WHERE media_id=?1",
        "UPDATE user_library_items SET resume_activity_at=999 WHERE media_id=?1",
        "DELETE FROM user_library_continue_units WHERE media_id=?1 AND episode=2",
    ] {
        let f = Fixture::new();
        let library = f.library("tv");
        let media = f.media();
        f.own(library, media, 1);
        f.own(library, media, 2);
        f.progress(media, 1, 10);
        f.progress(media, 2, 20);
        assert!(!f.integrity(library, false).has_issues());
        let raw = rusqlite::Connection::open(f.dir.path().join("data/library.db")).unwrap();
        raw.execute(damage, [media.to_string()]).unwrap();
        assert!(
            f.integrity(library, false).has_issues(),
            "damage escaped public Integrity: {damage}"
        );
        f.integrity(library, true);
        assert!(
            !f.integrity(library, false).has_issues(),
            "repair left projection damage: {damage}"
        );
        let mut query = f.query(&[library]);
        query.season = Some(1);
        let resume = f.store.resume_page(&query).unwrap();
        assert_eq!(episode(&resume.items[0].reference), (media, 2));
        assert_eq!(resume.items[0].last_played_at, 20);
        query.user_id = UserId::new();
        assert_eq!(
            episode(&f.store.next_up_page(&query).unwrap().items[0].reference),
            (media, 1)
        );
        let home = f
            .store
            .home_activity_page(&HomeQuery {
                user_id: f.user,
                libraries: vec![library],
                limit: 20,
            })
            .unwrap();
        assert_eq!(episode(&home[0].reference), (media, 2));
        assert!(!home[0].advanced);
    }
}

#[test]
fn clearing_and_deleting_user_state_leave_a_correct_unstarted_baseline() {
    let f = Fixture::new();
    let library = f.library("tv");
    let media = f.media();
    f.own(library, media, 1);
    f.own(library, media, 2);
    f.progress(media, 1, 10);
    f.store
        .mutate_playback(
            media,
            &PlaybackMutation {
                user_id: f.user,
                scope: UnitScope::Episode {
                    season: 1,
                    episode: 1,
                },
                played: None,
                favorite: Some(true),
                expected_generation: None,
            },
            20_000,
        )
        .unwrap();
    f.store.clear_units(f.user, Some(&[media]), None).unwrap();
    assert!(
        f.store
            .resume_page(&f.query(&[library]))
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        f.store
            .home_activity_page(&HomeQuery {
                user_id: f.user,
                libraries: vec![library],
                limit: 20
            })
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        episode(&f.store.next_up_page(&f.query(&[library])).unwrap().items[0].reference),
        (media, 1)
    );
    assert!(
        f.store
            .get_user_library_summary(f.user, library, media)
            .unwrap()
            .unwrap()
            .favorite
    );
    let admin = UserId::new();
    for (id, role, login) in [
        (admin, domain::UserRole::Admin, "admin"),
        (f.user, domain::UserRole::Member, "viewer"),
    ] {
        f.store
            .insert_user(&domain::User {
                id,
                login: login.into(),
                enabled: true,
                role,
            })
            .unwrap();
    }
    assert!(f.store.delete_user(f.user).unwrap());
    assert!(!f.integrity(library, false).has_issues());
    assert!(f.store.unit_state(f.user, media, 1, 1).unwrap().is_none());
    assert!(
        f.store
            .get_user_library_summary(f.user, library, media)
            .unwrap()
            .is_none()
    );
}

#[test]
fn merge_rebuilds_sparse_units_and_file_delete_clears_derived_candidates() {
    let f = Fixture::new();
    let library = f.library("tv");
    let survivor = f.media();
    let loser = f.media();
    let first = f.own(library, survivor, 1);
    let second = f.own(library, loser, 2);
    f.progress(survivor, 1, 10);
    f.progress(loser, 2, 30);
    let operation = f
        .store
        .begin_media_merge(
            &MediaMergeRequest {
                request_id: uuid::Uuid::new_v4().to_string(),
                survivor,
                loser,
                survivor_revision: 0,
                loser_revision: 0,
                refresh_targets: BTreeMap::from([("tmdb".into(), survivor.to_string())]),
            },
            1000,
        )
        .unwrap();
    assert_eq!(
        f.store.advance_media_merge(&operation, 2000).unwrap().state,
        "succeeded"
    );
    let resume = f.store.resume_page(&f.query(&[library])).unwrap();
    assert_eq!(resume.total, 1);
    assert_eq!(episode(&resume.items[0].reference), (survivor, 2));
    assert_eq!(resume.items[0].last_played_at, 30);
    assert!(!f.integrity(library, false).has_issues());
    f.store
        .delete_files(library, &[first.file_id, second.file_id])
        .unwrap();
    // Retired identities still reference their canonical survivor. Existing merge
    // retention keeps that identity, while all file-owned derived rows disappear.
    assert!(f.store.get_media(survivor).unwrap().is_some());
    assert!(
        f.store
            .next_up_page(&f.query(&[library]))
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        f.store
            .resume_page(&f.query(&[library]))
            .unwrap()
            .items
            .is_empty()
    );
    assert!(!f.integrity(library, false).has_issues());
}

#[test]
fn library_removal_keeps_shared_media_continuation_then_purges_last_owner() {
    let f = Fixture::new();
    let a = f.library("a");
    let b = f.library("b");
    let media = f.media();
    f.own(a, media, 1);
    f.own(b, media, 2);
    f.progress(media, 1, 10);
    let first = f.store.begin_library_removal(&a.to_string()).unwrap();
    let page = f.store.next_up_page(&f.query(&[a, b])).unwrap();
    assert_eq!(episode(&page.items[0].reference), (media, 2));
    assert_eq!(page.items[0].reference.library_id, b);
    f.store
        .set_removal_state(&first.operation_id, "cleaning", None)
        .unwrap();
    assert_eq!(
        f.store
            .clean_library_removal(&first.operation_id)
            .unwrap()
            .state,
        "succeeded"
    );
    assert!(!f.integrity(b, false).has_issues());
    assert!(f.store.get_media(media).unwrap().is_some());
    let last = f.store.begin_library_removal(&b.to_string()).unwrap();
    f.store
        .set_removal_state(&last.operation_id, "cleaning", None)
        .unwrap();
    assert_eq!(
        f.store
            .clean_library_removal(&last.operation_id)
            .unwrap()
            .state,
        "succeeded"
    );
    assert!(f.store.get_media(media).unwrap().is_none());
    assert!(f.store.unit_state(f.user, media, 1, 1).unwrap().is_none());
}

#[test]
fn ordinary_media_last_file_delete_purges_raw_progress_and_favorites() {
    let f = Fixture::new();
    let library = f.library("ordinary-orphan");
    let media = f.media();
    let file = f.own(library, media, 1);
    f.progress(media, 1, 10);
    f.store
        .mutate_playback(
            media,
            &PlaybackMutation {
                user_id: f.user,
                scope: UnitScope::Episode {
                    season: 1,
                    episode: 1,
                },
                played: None,
                favorite: Some(true),
                expected_generation: None,
            },
            20_000,
        )
        .unwrap();
    assert!(
        f.store
            .unit_state(f.user, media, 1, 1)
            .unwrap()
            .unwrap()
            .favorite
    );
    f.store.delete_files(library, &[file.file_id]).unwrap();
    assert!(f.store.get_media(media).unwrap().is_none());
    assert!(f.store.unit_state(f.user, media, 1, 1).unwrap().is_none());
    assert!(
        f.store
            .get_user_library_summary(f.user, library, media)
            .unwrap()
            .is_none()
    );
    assert!(
        f.store
            .resume_page(&f.query(&[library]))
            .unwrap()
            .items
            .is_empty()
    );
    assert!(!f.integrity(library, false).has_issues());
}

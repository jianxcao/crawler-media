use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use domain::{
    LibraryId, Media, MediaId, MediaKind, ProtocolItemQuery, ProtocolItemType, ProtocolParent,
    UnitKey, User, UserId, UserRole,
};
use rusqlite::{Connection, params};
use store::{
    LibraryReadRequest, LibraryReadResult, QueryBudget, ReadCredential, Store, stats::StatsScope,
};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    db: Connection,
    user: UserId,
    media: MediaId,
    library: LibraryId,
}

impl Fixture {
    fn new(episodes: i32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let user = UserId::new();
        let media = MediaId::new();
        store
            .insert_user(&User {
                id: user,
                login: "series-work".into(),
                enabled: true,
                role: UserRole::Admin,
            })
            .unwrap();
        store.set_user_token(user, "series-work-token").unwrap();
        store
            .insert_media(&Media {
                id: media,
                kind: MediaKind::Tv,
                title: "Series work".into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let library = store.default_library(MediaKind::Tv).unwrap().unwrap();
        let units: Vec<_> = (1..=episodes).map(|e| UnitKey::new(1, e)).collect();
        store::test_support::publish_fixture_file_units(
            &store,
            &store::test_support::FixtureFile {
                media_id: media,
                path: library.root_paths[0].join("pack.mkv").display().to_string(),
                ..Default::default()
            },
            &units,
        )
        .unwrap();
        let db = Connection::open(dir.path().join("library.db")).unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        Self {
            _dir: dir,
            store,
            db,
            user,
            media,
            library: library.id.parse().unwrap(),
        }
    }

    fn mark(
        &self,
        user: UserId,
        season: i32,
        episode: i32,
        played: bool,
        assertion: bool,
        time: i64,
    ) {
        self.db.execute("INSERT INTO playback_units(user_id,media_id,season,episode,played,has_played_assertion,played_changed_at_ms)
            VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(user_id,media_id,season,episode) DO UPDATE SET
            played=excluded.played,has_played_assertion=excluded.has_played_assertion,played_changed_at_ms=excluded.played_changed_at_ms",
            params![user.to_string(),self.media.to_string(),season,episode,played,assertion,assertion.then_some(time)]).unwrap();
    }

    fn query(&self) -> ProtocolItemQuery {
        ProtocolItemQuery {
            user_id: self.user,
            parent: ProtocolParent::Library(self.library),
            series_id: None,
            season: None,
            person_ids: vec![],
            studio_ids: vec![],
            search_term: None,
            genres: vec![],
            genre_ids: vec![],
            filters: vec![],
            is_favorite: None,
            is_played: None,
            include_types: vec![ProtocolItemType::Series],
            recursive: true,
            is_folder: None,
            sort: vec![],
            offset: 0,
            limit: 1,
        }
    }

    fn completed(&self) -> usize {
        self.store
            .protocol_series_scalar(
                self.user,
                self.media,
                &ProtocolParent::Library(self.library),
            )
            .unwrap()
            .completed
    }

    fn measured(&self, episodes: usize) -> (u64, u64) {
        let (scalar, work) = self.store.with_stats(StatsScope::Library, || {
            self.store
                .protocol_series_scalar(
                    self.user,
                    self.media,
                    &ProtocolParent::Library(self.library),
                )
                .unwrap()
        });
        assert_eq!((scalar.episodes, scalar.completed), (episodes, episodes));
        let pool = self
            .store
            .open_library_read_pool(NonZeroUsize::new(1).unwrap())
            .unwrap();
        let context = self
            .store
            .prepare_library_read(
                &ReadCredential::new(self.user, "series-work-token".into()),
                &LibraryReadRequest::Protocol(self.query()),
            )
            .unwrap();
        let budget = QueryBudget {
            deadline: Instant::now() + Duration::from_secs(30),
            cancelled: Default::default(),
        };
        let (result, leased) = pool
            .try_checkout()
            .unwrap()
            .execute_observed(&context, &budget);
        let LibraryReadResult::Protocol { page, series, .. } = result.unwrap() else {
            panic!("expected protocol page")
        };
        assert_eq!(page.total, 1);
        assert_eq!(page.items.len(), 1);
        assert_eq!(
            (series[&self.media].episodes, series[&self.media].completed),
            (episodes, episodes)
        );
        (work.vm_steps, leased.vm_steps)
    }
}

#[test]
fn series_work_is_linear_with_and_without_statistics() {
    let mut rows = Vec::new();
    for episodes in [100, 1000, 5000] {
        let f = Fixture::new(episodes);
        let tx = f.db.unchecked_transaction().unwrap();
        for episode in 1..=episodes {
            f.mark(f.user, 1, episode, true, true, 100);
        }
        tx.commit().unwrap();
        let fresh = f.measured(episodes as usize);
        f.db.execute_batch("ANALYZE").unwrap();
        let analyzed = f.measured(episodes as usize);
        println!("episodes={episodes} fresh={fresh:?} analyzed={analyzed:?}");
        rows.push((fresh, analyzed));
    }
    for (small, large, multiplier) in [(0, 1, 12), (1, 2, 6)] {
        for analyzed in [false, true] {
            let a = if analyzed {
                rows[small].1
            } else {
                rows[small].0
            };
            let b = if analyzed {
                rows[large].1
            } else {
                rows[large].0
            };
            assert!(
                b.0 <= multiplier * a.0,
                "scalar grew superlinearly: {rows:?}"
            );
            assert!(
                b.1 <= multiplier * a.1,
                "leased protocol grew superlinearly: {rows:?}"
            );
        }
    }
}

#[test]
fn series_assertions_preserve_explicit_false_ties_and_own_fallback() {
    let f = Fixture::new(4);
    assert_eq!(f.completed(), 0);
    f.mark(f.user, -1, -1, true, true, 10);
    assert_eq!(f.completed(), 4);
    f.mark(f.user, 1, -1, false, true, 10);
    assert_eq!(f.completed(), 0);
    f.mark(f.user, 1, 2, true, true, 10);
    f.mark(f.user, 1, 3, true, true, 9);
    f.mark(f.user, 1, 4, false, true, 11);
    f.mark(UserId::new(), -1, -1, true, true, 1000);
    assert_eq!(f.completed(), 1);
    f.mark(f.user, -1, -1, true, true, 12);
    assert_eq!(f.completed(), 4);
    f.db.execute(
        "DELETE FROM playback_units WHERE user_id=?1",
        [f.user.to_string()],
    )
    .unwrap();
    f.mark(f.user, 1, 1, false, false, 0);
    f.mark(f.user, 1, 2, false, true, 15);
    assert_eq!(f.completed(), 0);
}

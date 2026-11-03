//! Fixed episode pages must seek coverage; exact totals and offsets remain linear.
use domain::{
    LibraryId, Media, MediaId, MediaKind, ProtocolItemQuery, ProtocolItemType, ProtocolParent,
    ProtocolSort, ProtocolSortKey, TypedItemId, UnitKey, User, UserId, UserRole,
};
use rusqlite::{Connection, params};
use store::{Store, library_query::items::QueryWorkStats, stats::StatsScope};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    db: Connection,
    library: LibraryId,
    media: MediaId,
    user: UserId,
    root_id: String,
    root: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let user = UserId::new();
        store
            .insert_user(&User {
                id: user,
                login: "episode-page-work".into(),
                enabled: true,
                role: UserRole::Admin,
            })
            .unwrap();
        let media = MediaId::new();
        store
            .insert_media(&Media {
                id: media,
                kind: MediaKind::Tv,
                title: "Episode page".into(),
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
        let db = Connection::open(dir.path().join("library.db")).unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        let root_id = db
            .query_row(
                "SELECT root_id FROM library_roots WHERE library_id=?1",
                [&library.id],
                |r| r.get(0),
            )
            .unwrap();
        Self {
            _dir: dir,
            store,
            db,
            library: library.id.parse().unwrap(),
            media,
            user,
            root_id,
            root: library.root_paths[0].display().to_string(),
        }
    }

    fn files(&self, season: i32, first: i32, last: i32) {
        let tx = self.db.unchecked_transaction().unwrap();
        for episode in first..=last {
            let file = domain::LedgerId::new().to_string();
            let path = format!("{}/{season}-{episode}.mkv", self.root);
            tx.execute("INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,presence_state,publication_state,file_revision,source_version,quality_source,confidence) VALUES(?1,?2,?3,?4,'tv',?5,?5,'present','published',1,'v1','probe','high')", params![file,self.library.to_string(),self.root_id,self.media.to_string(),path]).unwrap();
            tx.execute("INSERT INTO library_file_units(file_id,library_id,media_id,season,episode) VALUES(?1,?2,?3,?4,?5)", params![file,self.library.to_string(),self.media.to_string(),season,episode]).unwrap();
        }
        tx.commit().unwrap();
        let operation = format!("repair-{season}-{last}");
        self.store
            .begin_library_integrity(self.library, true, &operation, 0)
            .unwrap();
        loop {
            let state = self
                .store
                .advance_library_operation(&operation, 1, 100)
                .unwrap();
            if state.state != "applying" {
                assert_eq!(state.state, "succeeded");
                break;
            }
        }
    }

    fn query(&self, limit: usize) -> ProtocolItemQuery {
        ProtocolItemQuery {
            user_id: self.user,
            parent: ProtocolParent::Season(self.media, 1),
            series_id: Some(self.media),
            season: Some((self.media, 1)),
            person_ids: vec![],
            studio_ids: vec![],
            search_term: None,
            genres: vec![],
            genre_ids: vec![],
            filters: vec![],
            is_favorite: None,
            is_played: None,
            include_types: vec![ProtocolItemType::Episode],
            recursive: true,
            is_folder: None,
            sort: vec![ProtocolSort {
                key: ProtocolSortKey::EpisodeNumber,
                descending: false,
            }],
            offset: 0,
            limit,
        }
    }

    fn measure(&self, episodes: u64) -> (QueryWorkStats, QueryWorkStats) {
        let (count, count_work) = self.store.with_stats(StatsScope::Library, || {
            self.store.protocol_item_page(&self.query(0))
        });
        let count = count.unwrap();
        assert_eq!(count.total, episodes);
        assert!(count.items.is_empty());
        let (page, page_work) = self.store.with_stats(StatsScope::Library, || {
            self.store.protocol_item_page(&self.query(50))
        });
        let page = page.unwrap();
        assert_eq!(page.total, episodes);
        assert_eq!(page.items.len(), 50);
        for (index, item) in page.items.iter().enumerate() {
            assert_eq!(
                item.item,
                TypedItemId::Unit {
                    media: self.media,
                    key: UnitKey::new(1, index as i32 + 1)
                }
            );
            assert_eq!(item.library_id, self.library);
            assert_eq!(item.preferred_source.as_ref().unwrap().revision, 1);
        }
        println!("episodes={episodes} count={count_work:?} page={page_work:?}");
        (count_work, page_work)
    }
}

#[test]
fn pinned_episode_scope_does_not_enumerate_unrelated_season_files() {
    let f = Fixture::new();
    f.files(1, 1, 100);
    let (small_count, _) = f.measure(100);
    f.files(2, 1, 1200);
    let (large_count, _) = f.measure(100);
    assert!(
        large_count.vm_steps <= small_count.vm_steps + 1000,
        "fixed-season exact count must not enumerate other-season files: {small_count:?} -> {large_count:?}"
    );
}

#[test]
fn fixed_episode_page_supplement_is_bounded_as_same_media_files_grow() {
    let f = Fixture::new();
    f.files(1, 1, 100);
    let (small_count, small_page) = f.measure(100);
    f.files(1, 101, 1500);
    let (large_count, large_page) = f.measure(1500);
    // Both public calls calculate the same exact total. Subtraction isolates the
    // additional page selection/source work without pretending counting is O(1).
    let small = small_page.vm_steps - small_count.vm_steps;
    let large = large_page.vm_steps - large_count.vm_steps;
    assert!(
        large <= small + 2500,
        "50 episode sources/page selection must not rescan all Media files/units: small={small}, large={large}"
    );
}

#[test]
fn episode_sources_keep_actual_revision_ties_coverage_and_availability() {
    let f = Fixture::new();
    f.files(1, 1, 3);
    let mut versions = [domain::LedgerId::new(), domain::LedgerId::new()];
    versions.sort_by_key(ToString::to_string);
    for id in versions {
        let path = format!("{}/version-{id}.mkv", f.root);
        f.db.execute("INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,presence_state,publication_state,file_revision,source_version,quality_source,confidence) VALUES(?1,?2,?3,?4,'tv',?5,?5,'present','published',10,'v10','probe','high')", params![id.to_string(),f.library.to_string(),f.root_id,f.media.to_string(),path]).unwrap();
        for episode in [1, 2] {
            f.db.execute("INSERT INTO library_file_units(file_id,library_id,media_id,season,episode) VALUES(?1,?2,?3,1,?4)", params![id.to_string(),f.library.to_string(),f.media.to_string(),episode]).unwrap();
        }
    }
    let mut query = f.query(3);
    let sources = || {
        let page = f.store.protocol_item_page(&query).unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(page.items.len(), 3);
        page.items
            .into_iter()
            .map(|item| item.preferred_source)
            .collect::<Vec<_>>()
    };
    let first = sources();
    assert_eq!(first[0].as_ref().unwrap().file_id, versions[0]);
    assert_eq!(first[1], first[0], "one file may cover multiple episodes");
    assert_eq!(first[0].as_ref().unwrap().revision, 10);
    assert_eq!(first[2].as_ref().unwrap().revision, 1);
    f.db.execute(
        "UPDATE library_files SET publication_state='pending' WHERE file_id=?1",
        [versions[0].to_string()],
    )
    .unwrap();
    assert_eq!(sources()[0].as_ref().unwrap().file_id, versions[1]);
    f.db.execute(
        "UPDATE library_files SET file_revision=99,presence_state='missing' WHERE file_id=?1",
        [versions[1].to_string()],
    )
    .unwrap();
    let fallback = sources();
    assert_eq!(fallback[0].as_ref().unwrap().revision, 1);
    assert_ne!(
        fallback[0], fallback[1],
        "coverage must stay episode-specific"
    );
    f.db.execute(
        "UPDATE library_files SET presence_state='unknown' WHERE file_id=?1",
        [fallback[0].as_ref().unwrap().file_id.to_string()],
    )
    .unwrap();
    assert_eq!(
        sources()[0],
        None,
        "an unavailable unit keeps its logical row"
    );
    // Explicit descending order keeps the general ordering semantics.
    query.sort[0].descending = true;
    let page = f.store.protocol_item_page(&query).unwrap();
    assert_eq!(
        page.items[0].item,
        TypedItemId::Unit {
            media: f.media,
            key: UnitKey::new(1, 3)
        }
    );
}

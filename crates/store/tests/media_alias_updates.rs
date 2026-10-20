use domain::{
    Confidence, Coverage, FilePresence, FilePublication, FileVersion,
    LedgerId, LibraryFile, Media, MediaAlias, MediaId, MediaKind, QualitySource,
    Subscribe, SubscribeId, UnitKey, UserId,
};
use rusqlite::params;
use store::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::{Store, StoreError};

fn make_media(title: &str, year: Option<u16>) -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: title.into(),
        year,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn create_library(store: &Store, name: &str, kind: MediaKind, root_path: &str) -> (store::Library, String) {
    let lib = store
        .create_library(kind, name, &[root_path], "everyone", true, &[])
        .unwrap();
    let roots = store.list_library_roots().unwrap();
    let root = roots.into_iter().find(|r| r.library_id == Some(lib.id.clone())).unwrap();
    (lib, root.id)
}

#[test]
fn identity_edit_preserves_multiple_aliases_and_selected_refresh_target() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let db = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();

    let mut m = make_media("Before", Some(2020));
    m.tmdb_id = Some("100".into());
    store.insert_media(&m).unwrap();

    // 1. Add second alias for same source ("tmdb"), external_id = "999"
    let alias2 = MediaAlias {
        source: "tmdb".into(),
        kind: MediaKind::Tv,
        external_id: "999".into(),
    };
    store.add_media_alias(m.id, &alias2).unwrap();

    // Both aliases can be resolved to m.id
    assert_eq!(
        store.get_media_by_alias_kind("tmdb", "100", Some(MediaKind::Tv)).unwrap().unwrap().id,
        m.id
    );
    assert_eq!(
        store.get_media_by_alias_kind("tmdb", "999", Some(MediaKind::Tv)).unwrap().unwrap().id,
        m.id
    );

    // Initial refresh target is the first one inserted ("100")
    let target_before = store.media_refresh_alias(m.id, "tmdb").unwrap();
    assert_eq!(target_before.as_ref().map(|a| a.external_id.as_str()), Some("100"));

    // Set refresh target to "999" directly in media_aliases
    db.execute(
        "UPDATE media_aliases SET is_refresh_target = CASE WHEN external_id = '999' THEN 1 ELSE 0 END WHERE media_id = ?1 AND source = 'tmdb'",
        params![m.id.to_string()],
    ).unwrap();

    let refresh_target = store.media_refresh_alias(m.id, "tmdb").unwrap().unwrap();
    assert_eq!(refresh_target.external_id, "999");

    // Also set up a library item to test card update
    let (library, root_id) = create_library(&store, "TV Shows", MediaKind::Tv, "/shows");
    let scope = WriteScope {
        library_id: library.id.parse().unwrap(),
        config_revision: 0,
    };
    let file = LibraryFile {
        file_id: LedgerId::new(),
        library_id: library.id.parse().unwrap(),
        root_id,
        path: std::path::PathBuf::from("/shows/Show.mkv"),
        media_id: m.id,
        kind: MediaKind::Tv,
        version: FileVersion { revision: 1, source_version: "v1".into() },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(100),
        stat_known: true,
        created_ns: Some(0),
        modified_ns: Some(0),
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    store.publish_files(&scope, &[StorePublication {
        file: &file,
        units: &[UnitKey::WHOLE],
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    }], 1000).unwrap();

    // Check card before
    let page = store.library_item_page(&LibraryQuery {
        library_id: scope.library_id,
        user_id: None,
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset { start: 0, limit: 10 },
        include_total: true,
    }).unwrap();
    assert_eq!(page.items.len(), 1);
    let card_before = &page.items[0];
    assert_eq!(card_before.title, "Before");
    let item_rev_before: i64 = db.query_row(
        "SELECT item_revision FROM library_items WHERE library_id = ?1 AND media_id = ?2",
        params![scope.library_id.to_string(), m.id.to_string()],
        |r| r.get(0),
    ).unwrap();

    // Check alias count before
    let alias_ids_before: Vec<String> = {
        let mut stmt = db.prepare("SELECT external_id FROM media_aliases WHERE media_id = ?1 ORDER BY external_id").unwrap();
        stmt.query_map([m.id.to_string()], |r| r.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap()
    };
    assert_eq!(alias_ids_before, vec!["100", "999"]);

    // Update media identity to "After", year 2021
    let mut updated_media = m.clone();
    updated_media.title = "After".into();
    updated_media.year = Some(2021);

    // Call update_media_identity(expected_revision, &updated_media)
    let next_rev = store.update_media_identity(0, &updated_media).unwrap();
    assert_eq!(next_rev, 1);

    // Verify alias ids after
    let alias_ids_after: Vec<String> = {
        let mut stmt = db.prepare("SELECT external_id FROM media_aliases WHERE media_id = ?1 ORDER BY external_id").unwrap();
        stmt.query_map([m.id.to_string()], |r| r.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap()
    };
    assert_eq!(alias_ids_after, alias_ids_before);

    // Refresh target still preserved!
    let refresh_target = store.media_refresh_alias(m.id, "tmdb").unwrap().unwrap();
    assert_eq!(refresh_target.external_id, "999");

    // Both aliases still resolve
    assert_eq!(
        store.get_media_by_alias_kind("tmdb", "100", Some(MediaKind::Tv)).unwrap().unwrap().id,
        m.id
    );
    assert_eq!(
        store.get_media_by_alias_kind("tmdb", "999", Some(MediaKind::Tv)).unwrap().unwrap().id,
        m.id
    );

    // Verify card is updated
    let page_after = store.library_item_page(&LibraryQuery {
        library_id: scope.library_id,
        user_id: None,
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset { start: 0, limit: 10 },
        include_total: true,
    }).unwrap();
    let card = &page_after.items[0];
    assert_eq!(card.title, "After");
    assert_eq!(card.year, Some(2021));
    let item_rev_after: i64 = db.query_row(
        "SELECT item_revision FROM library_items WHERE library_id = ?1 AND media_id = ?2",
        params![scope.library_id.to_string(), m.id.to_string()],
        |r| r.get(0),
    ).unwrap();
    assert!(item_rev_after > item_rev_before);

    // CAS check on update_media_identity
    let err = store.update_media_identity(0, &updated_media).unwrap_err();
    assert!(matches!(err, StoreError::Protected(_)));

    // Now test frozen states: merging and retired
    db.execute(
        "UPDATE media SET identity_state = 'merging' WHERE media_id = ?1",
        params![m.id.to_string()],
    ).unwrap();

    let frozen_state_before: String = db.query_row(
        "SELECT identity_state FROM media WHERE media_id = ?1",
        params![m.id.to_string()],
        |r| r.get(0),
    ).unwrap();
    assert_eq!(frozen_state_before, "merging");

    // Attempt to update identity while merging -> rejected
    let update_err = store.update_media_identity(1, &updated_media).unwrap_err();
    assert!(matches!(update_err, StoreError::Protected(_)));

    // Attempt to insert subscribe on frozen media -> rejected
    let sub = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: m.id,
        coverage: Coverage::Tv { season: 1, episode_from: 1, episode_to: None },
        fetch_mode: domain::FetchMode::Search,
        filter_id: domain::FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 3600,
        keep_old_versions: false,
        library_id: None,
    };
    let sub_err = store.insert_subscribe(&sub).unwrap_err();
    assert!(matches!(sub_err, StoreError::Protected(_)));

    // Attempt to publish files on frozen media -> rejected
    let pub_err = store.publish_files(&scope, &[StorePublication {
        file: &file,
        units: &[UnitKey::WHOLE],
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    }], 2000).unwrap_err();
    assert!(matches!(pub_err, StoreError::Protected(_)));

    let rejected_frozen_state: String = db.query_row(
        "SELECT identity_state FROM media WHERE media_id = ?1",
        params![m.id.to_string()],
        |r| r.get(0),
    ).unwrap();
    assert_eq!(rejected_frozen_state, frozen_state_before);
}

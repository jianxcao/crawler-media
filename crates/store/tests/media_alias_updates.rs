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

/// Builds a Tv Media with two `tmdb` aliases ("100" first, then "999"), switches the
/// refresh target to "999" and publishes one library item, so both the alias rows and the
/// card snapshot in `library_items` exist before an identity edit.
struct AliasFixture {
    media: Media,
    scope: WriteScope,
    file: LibraryFile,
    card_aliases_before: String,
    item_rev_before: i64,
}

fn setup_alias_fixture(store: &Store, db: &rusqlite::Connection) -> AliasFixture {
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
    let (library, root_id) = create_library(store, "TV Shows", MediaKind::Tv, "/shows");
    let scope = WriteScope {
        library_id: library.id.parse().unwrap(),
        config_revision: 0,
    };
    let file = make_file(&library.id, root_id, m.id);
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
    assert_eq!(page.items[0].title, "Before");
    let item_rev_before: i64 = db.query_row(
        "SELECT item_revision FROM library_items WHERE library_id = ?1 AND media_id = ?2",
        params![scope.library_id.to_string(), m.id.to_string()],
        |r| r.get(0),
    ).unwrap();

    assert_eq!(read_alias_ids(db, m.id), vec!["100", "999"]);
    // Card alias snapshot before the identity edit: the same commit must keep it.
    let card_aliases_before = read_card_aliases(db, &scope, m.id);
    AliasFixture { media: m, scope, file, card_aliases_before, item_rev_before }
}

fn make_file(library_id: &str, root_id: String, media_id: MediaId) -> LibraryFile {
    LibraryFile {
        file_id: LedgerId::new(),
        library_id: library_id.parse().unwrap(),
        root_id,
        path: std::path::PathBuf::from("/shows/Show.mkv"),
        media_id,
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
    }
}

fn retitled(media: &Media) -> Media {
    let mut updated = media.clone();
    updated.title = "After".into();
    updated.year = Some(2021);
    updated
}

fn read_alias_ids(db: &rusqlite::Connection, media_id: MediaId) -> Vec<String> {
    let mut stmt = db
        .prepare("SELECT external_id FROM media_aliases WHERE media_id = ?1 ORDER BY external_id")
        .unwrap();
    stmt.query_map([media_id.to_string()], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn read_card_aliases(db: &rusqlite::Connection, scope: &WriteScope, media_id: MediaId) -> String {
    db.query_row(
        "SELECT aliases_json FROM library_items WHERE library_id = ?1 AND media_id = ?2",
        params![scope.library_id.to_string(), media_id.to_string()],
        |r| r.get(0),
    ).unwrap()
}

#[test]
fn identity_edit_preserves_multiple_aliases_and_selected_refresh_target() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let db = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();
    let fx = setup_alias_fixture(&store, &db);

    // Update media identity to "After", year 2021
    let next_rev = store.update_media_identity(0, &retitled(&fx.media)).unwrap();
    assert_eq!(next_rev, 1);

    // Verify alias ids after
    assert_eq!(read_alias_ids(&db, fx.media.id), vec!["100", "999"]);

    // Refresh target still preserved!
    let refresh_target = store.media_refresh_alias(fx.media.id, "tmdb").unwrap().unwrap();
    assert_eq!(refresh_target.external_id, "999");

    // Both aliases still resolve
    assert_eq!(
        store.get_media_by_alias_kind("tmdb", "100", Some(MediaKind::Tv)).unwrap().unwrap().id,
        fx.media.id
    );
    assert_eq!(
        store.get_media_by_alias_kind("tmdb", "999", Some(MediaKind::Tv)).unwrap().unwrap().id,
        fx.media.id
    );

    // Verify card is updated
    let page_after = store.library_item_page(&LibraryQuery {
        library_id: fx.scope.library_id,
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
        params![fx.scope.library_id.to_string(), fx.media.id.to_string()],
        |r| r.get(0),
    ).unwrap();
    assert!(item_rev_after > fx.item_rev_before);

    // 3a: the card snapshot (title / year / alias JSON / item_revision) is rebuilt in the
    // same commit as the identity edit — read straight from the persisted row, not a view.
    let (card_title, card_year, card_aliases_after, item_rev_row): (String, Option<i64>, String, i64) =
        db.query_row(
            "SELECT title, year, aliases_json, item_revision FROM library_items WHERE library_id = ?1 AND media_id = ?2",
            params![fx.scope.library_id.to_string(), fx.media.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).unwrap();
    assert_eq!(card_title, "After");
    assert_eq!(card_year, Some(2021));
    assert_eq!(item_rev_row, item_rev_after);
    // Both aliases are still present in the card's alias JSON (the object is rebuilt from
    // media_aliases in the same transaction), so an edit cannot drop the second alias.
    assert_eq!(card_aliases_after, fx.card_aliases_before);
    let alias_json: serde_json::Value = serde_json::from_str(&card_aliases_after).unwrap();
    assert!(
        alias_json["tmdb"].is_string(),
        "card alias JSON must keep the tmdb alias: {card_aliases_after}"
    );
}

#[test]
fn identity_edit_with_a_stale_revision_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let db = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();
    let fx = setup_alias_fixture(&store, &db);

    store.update_media_identity(0, &retitled(&fx.media)).unwrap();

    // CAS check on update_media_identity: the second edit still expects revision 0.
    let err = store.update_media_identity(0, &retitled(&fx.media)).unwrap_err();
    assert!(matches!(err, StoreError::Protected(_)));
}

#[test]
fn frozen_media_rejects_identity_edits_and_card_alias_drift() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let db = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();
    let fx = setup_alias_fixture(&store, &db);

    store.update_media_identity(0, &retitled(&fx.media)).unwrap();
    let card_aliases_after = read_card_aliases(&db, &fx.scope, fx.media.id);

    // Frozen states: both `merging` and `retired` must reject every write and leave the row
    // (`media.identity_state` / `media_aliases` / card alias JSON) untouched. `retired`
    // requires a survivor because of CHECK((identity_state='retired')=(survivor_id IS NOT NULL)).
    let survivor = make_media("Survivor", None);
    store.insert_media(&survivor).unwrap();
    assert_frozen_media_rejects_writes(
        &store, &db, &fx.media, &fx.scope, &fx.file, card_aliases_after.as_str(), "merging", None,
    );
    assert_frozen_media_rejects_writes(
        &store, &db, &fx.media, &fx.scope, &fx.file, card_aliases_after.as_str(), "retired", Some(&survivor),
    );
}

/// A frozen (merging/retired) Media must reject identity edits, new Subscribes and file
/// publications, and each rejection must leave `media` and the card alias JSON unchanged.
fn assert_frozen_media_rejects_writes(
    store: &Store,
    db: &rusqlite::Connection,
    media: &Media,
    scope: &WriteScope,
    file: &LibraryFile,
    card_aliases_before: &str,
    state: &str,
    survivor: Option<&Media>,
) {
    db.execute(
        "UPDATE media SET identity_state = ?2, survivor_id = ?3 WHERE media_id = ?1",
        params![media.id.to_string(), state, survivor.map(|s| s.id.to_string())],
    ).unwrap();

    let frozen_state_before: String = db.query_row(
        "SELECT identity_state FROM media WHERE media_id = ?1",
        params![media.id.to_string()],
        |r| r.get(0),
    ).unwrap();
    assert_eq!(frozen_state_before, state);

    let current = store.get_media(media.id).unwrap().unwrap();
    let rev = store.media_revision(media.id).unwrap().unwrap();

    // Identity edit while frozen -> rejected
    let update_err = store.update_media_identity(rev, &current).unwrap_err();
    assert!(matches!(update_err, StoreError::Protected(_)), "{state}: {update_err}");

    // New Subscribe on a frozen media -> rejected
    let sub = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media.id,
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
    assert!(matches!(sub_err, StoreError::Protected(_)), "{state}: {sub_err}");

    // Publishing a file onto a frozen media -> rejected
    let pub_err = store.publish_files(scope, &[StorePublication {
        file,
        units: &[UnitKey::WHOLE],
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    }], 2000).unwrap_err();
    assert!(matches!(pub_err, StoreError::Protected(_)), "{state}: {pub_err}");

    let rejected_frozen_state: String = db.query_row(
        "SELECT identity_state FROM media WHERE media_id = ?1",
        params![media.id.to_string()],
        |r| r.get(0),
    ).unwrap();
    assert_eq!(rejected_frozen_state, frozen_state_before);

    let alias_ids: Vec<String> = {
        let mut stmt = db
            .prepare("SELECT external_id FROM media_aliases WHERE media_id = ?1 ORDER BY external_id")
            .unwrap();
        stmt.query_map([media.id.to_string()], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(alias_ids, vec!["100", "999"]);

    let card_aliases: String = db.query_row(
        "SELECT aliases_json FROM library_items WHERE library_id = ?1 AND media_id = ?2",
        params![scope.library_id.to_string(), media.id.to_string()],
        |r| r.get(0),
    ).unwrap();
    assert_eq!(card_aliases, card_aliases_before);
}

#[test]
fn refresh_target_read_follows_the_media_kind() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let db = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();

    let mut m = make_media("Show", Some(2020));
    m.tmdb_id = None;
    store.insert_media(&m).unwrap();

    // A `tmdb` target row of another kind, written with a lower rowid than the matching
    // row. The `(media_id,kind)` foreign key is switched off for this raw connection only,
    // so this defensive case is reachable (hand-edited / legacy databases) even though the
    // Store itself never writes it.
    db.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
    db.execute(
        "INSERT INTO media_aliases(source, kind, external_id, media_id, is_refresh_target) \
         VALUES('tmdb', 'movie', '999', ?1, 1)",
        params![m.id.to_string()],
    ).unwrap();
    db.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    store.add_media_alias(m.id, &MediaAlias {
        source: "tmdb".into(),
        kind: MediaKind::Tv,
        external_id: "100".into(),
    }).unwrap();

    let alias_ids: Vec<String> = {
        let mut stmt = db
            .prepare("SELECT external_id FROM media_aliases WHERE media_id = ?1 ORDER BY external_id")
            .unwrap();
        stmt.query_map([m.id.to_string()], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(alias_ids, vec!["100", "999"]);

    // The refresh target read must never return the other kind's row.
    let target = store.media_refresh_alias(m.id, "tmdb").unwrap().unwrap();
    assert_eq!(target.kind, MediaKind::Tv);
    assert_eq!(target.external_id, "100");
}

/// R22: a retained non-target alias (100, while the refresh target is 999) still identifies
/// the Media when a scan reports tmdb=100; an unregistered different id still conflicts.
#[test]
fn registered_non_target_alias_resolves_scanned_media() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let fixture = setup_alias_fixture(&store, &db);
    let canonical_id = fixture.media.id;

    let mut incoming = make_media("Before", Some(2020));
    incoming.tmdb_id = Some("100".into());
    let resolved_non_target = store.resolve_scanned_media(incoming).unwrap();
    assert_eq!(resolved_non_target.id, canonical_id);
    let target = store.media_refresh_alias(canonical_id, "tmdb").unwrap().unwrap();
    assert_eq!(target.external_id, "999", "scan must not move the refresh target");

    let mut foreign = make_media("Before", Some(2020));
    foreign.tmdb_id = Some("100".into());
    foreign.douban_id = Some("d-1".into());
    store.add_media_alias(canonical_id, &MediaAlias {
        source: "douban".into(), kind: MediaKind::Tv, external_id: "d-0".into(),
    }).unwrap();
    assert!(matches!(store.resolve_scanned_media(foreign), Err(StoreError::Protected(_))));
}

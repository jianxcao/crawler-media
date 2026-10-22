use domain::{Media, MediaId, MediaKind};
use store::{Store, StoreError};
fn media(title: &str, tmdb: Option<&str>) -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: title.into(),
        year: Some(2022),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: tmdb.map(str::to_owned),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}
#[test]
fn external_alias_reuses_identity_despite_title_change_and_keeps_kind_separate() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let old = media("Old title", Some("1"));
    store.insert_media(&old).unwrap();
    assert_eq!(
        store
            .resolve_scanned_media(media("新标题", Some("1")))
            .unwrap()
            .id,
        old.id
    );
    let mut movie = media("新标题", Some("1"));
    movie.kind = MediaKind::Movie;
    assert_ne!(store.resolve_scanned_media(movie).unwrap().id, old.id);
}
#[test]
fn conflicting_aliases_fail_without_mutating_existing_rows() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let first = media("Same", Some("1"));
    let mut second = media("Other", Some("2"));
    second.tvdb_id = Some("9".into());
    store.insert_media(&first).unwrap();
    store.insert_media(&second).unwrap();
    let mut incoming = media("Same", Some("1"));
    incoming.tvdb_id = Some("9".into());
    assert!(matches!(
        store.resolve_scanned_media(incoming),
        Err(StoreError::Protected(_))
    ));
    assert_eq!(store.get_media(first.id).unwrap().unwrap(), first);
    assert_eq!(store.get_media(second.id).unwrap().unwrap(), second);
    let different = store
        .resolve_scanned_media(media("Same", Some("3")))
        .unwrap();
    assert_ne!(different.id, first.id);
}
#[test]
fn duplicate_alias_and_single_row_alias_contradictions_are_protected() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let mut old = media("First", Some("1"));
    old.tvdb_id = Some("7".into());
    store.insert_media(&old).unwrap();
    let mut incoming = media("Renamed", Some("1"));
    incoming.tvdb_id = Some("8".into());
    assert!(matches!(
        store.resolve_scanned_media(incoming),
        Err(StoreError::Protected(_))
    ));
    let duplicate_err = store
        .insert_media(&media("Second", Some("1")))
        .expect_err("duplicate alias must fail");
    assert!(matches!(
        duplicate_err,
        StoreError::Protected(_) | StoreError::Sqlite(_)
    ));
    assert_eq!(store.get_media(old.id).unwrap().unwrap(), old);
}

#[test]
fn title_only_recovery_requires_unique_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let old = media("Same", Some("1"));
    store.insert_media(&old).unwrap();
    assert_eq!(
        store.resolve_scanned_media(media("Same", None)).unwrap().id,
        old.id
    );
    store.insert_media(&media("Same", Some("2"))).unwrap();
    assert!(matches!(
        store.resolve_scanned_media(media("Same", None)),
        Err(StoreError::Protected(_))
    ));
}

#[test]
fn missing_year_incoming_does_not_merge_into_existing_with_year() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let old = media("Same", Some("1"));
    store.insert_media(&old).unwrap();

    let mut incoming_no_year = media("Same", None);
    incoming_no_year.year = None;
    let resolved = store.resolve_scanned_media(incoming_no_year).unwrap();
    assert_ne!(resolved.id, old.id);
}

#[test]
fn incoming_with_different_alias_provider_does_not_merge_by_title() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let mut old = media("Same", None);
    old.douban_id = Some("douban-123".into());
    store.insert_media(&old).unwrap();

    let mut incoming = media("Same", None);
    incoming.tmdb_id = Some("tmdb-456".into());
    let resolved = store.resolve_scanned_media(incoming).unwrap();
    assert_ne!(resolved.id, old.id);
}

#[test]
fn frozen_media_is_never_revived_by_a_scan() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let db = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();

    let frozen = media("Frozen", Some("1"));
    store.insert_media(&frozen).unwrap();
    let survivor = media("Survivor", Some("2"));
    store.insert_media(&survivor).unwrap();

    // `retired` needs a survivor: CHECK((identity_state='retired')=(survivor_id IS NOT NULL)).
    for (state, survivor_id) in [
        ("merging", None),
        ("retired", Some(survivor.id.to_string())),
    ] {
        db.execute(
            "UPDATE media SET identity_state = ?2, survivor_id = ?3 WHERE media_id = ?1",
            rusqlite::params![frozen.id.to_string(), state, survivor_id],
        )
        .unwrap();

        // The scan still sees the tmdb alias, but must not adopt the frozen identity...
        let alias_err = store
            .resolve_scanned_media(media("Frozen", Some("1")))
            .unwrap_err();
        assert!(
            matches!(alias_err, StoreError::Protected(_)),
            "{state}: alias path returned {alias_err}"
        );

        // ...nor revive it through the title-only path (both rows carry the same year).
        let title_err = store
            .resolve_scanned_media(media("Frozen", None))
            .unwrap_err();
        assert!(
            matches!(title_err, StoreError::Protected(_)),
            "{state}: title path returned {title_err}"
        );

        let state_after: (String, Option<String>) = db
            .query_row(
                "SELECT identity_state, survivor_id FROM media WHERE media_id = ?1",
                rusqlite::params![frozen.id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state_after.0, state);
        let unchanged = store.get_media(frozen.id).unwrap().unwrap();
        assert_eq!(unchanged.title, "Frozen");
        assert_eq!(unchanged.year, Some(2022));
    }

    // No scan may insert a duplicate row for the frozen identity.
    assert_eq!(store.list_media().unwrap().len(), 2);
}

#[test]
fn frozen_media_rejects_scan_adoption_and_revert() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let db = rusqlite::Connection::open(temp.path().join("library.db")).unwrap();

    let frozen = media("Frozen", Some("1"));
    store.insert_media(&frozen).unwrap();
    let survivor = media("Survivor", Some("2"));
    store.insert_media(&survivor).unwrap();

    for (state, survivor_id) in [
        ("merging", None),
        ("retired", Some(survivor.id.to_string())),
    ] {
        db.execute(
            "UPDATE media SET identity_state = ?2, survivor_id = ?3 WHERE media_id = ?1",
            rusqlite::params![frozen.id.to_string(), state, survivor_id],
        )
        .unwrap();

        // Scan adoption by alias must be refused before `update_media` can rewrite the row.
        let adopt_err = store.ensure_media(media("Frozen", Some("1"))).unwrap_err();
        assert!(
            matches!(adopt_err, StoreError::Protected(_)),
            "{state}: adopt returned {adopt_err}"
        );

        // A revert must not resurrect a frozen identity either.
        let expected = store.get_media(frozen.id).unwrap().unwrap();
        let mut replacement = expected.clone();
        replacement.title = "Reverted".into();
        let revert = store.update_media_if_matches(&expected, &replacement);
        assert!(
            matches!(revert, Err(StoreError::Protected(_))),
            "{state}: revert returned {revert:?}"
        );

        let row = store.get_media(frozen.id).unwrap().unwrap();
        assert_eq!(row.title, "Frozen");
    }
}

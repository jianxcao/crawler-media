use domain::{Coverage, FetchMode, Media, MediaId, MediaKind, Subscribe, SubscribeId, UserId};
use rusqlite::{Connection, Error, ffi};
use store::Store;

#[test]
fn subscribe_insert_requires_both_timestamps() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let db = Connection::open(tmp.path().join("subscribe.db")).unwrap();
    for (columns, values) in [
        ("", ""),
        (", created_at", ", '2026-10-27T00:00:00Z'"),
        (", updated_at", ", '2026-10-27T00:00:00Z'"),
    ] {
        let error = db
            .execute(
                &format!(
                    "INSERT INTO subscribes
                     (id, user_id, media_id, coverage_kind, fetch_mode, filter_id{columns})
                     VALUES ('missing', 'user', 'media', 'movie', 'search', 'filter'{values})"
                ),
                [],
            )
            .expect_err("omitting either timestamp must fail");
        assert!(
            matches!(error, Error::SqliteFailure(code, _) if code.extended_code == ffi::SQLITE_CONSTRAINT_NOTNULL),
            "expected a NOT NULL refusal, got {error}"
        );
    }
    assert!(store.list_all_subscribes().unwrap().is_empty());
}

#[test]
fn subscribe_times_rejects_a_missing_subscribe() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    assert!(
        store.subscribe_times(SubscribeId::new()).is_err(),
        "a missing Subscribe must not produce empty timestamps"
    );
}

fn insert_subscribe(store: &Store) -> Subscribe {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Timestamp fixture".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        bangumi_id: None,
        anilist_id: None,
        tvdb_id: None,
    };
    store.insert_media(&media).unwrap();
    let subscribe = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: store.list_filters().unwrap()[0].id,
        wash_cut: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
        keep_old_versions: false,
    };
    store.insert_subscribe(&subscribe).unwrap();
    subscribe
}

#[test]
fn subscribe_timestamps_survive_reopen_and_update_preserves_creation_time() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let mut subscribe = insert_subscribe(&store);
    let inserted = store.subscribe_times(subscribe.id).unwrap();
    let before = "2000-01-01T00:00:00Z";
    for timestamp in [&inserted.0, &inserted.1] {
        assert!(
            timestamp.as_str() > before,
            "timestamp must be populated: {timestamp}"
        );
        assert!(timestamp.ends_with('Z'), "timestamp must use RFC3339 UTC");
    }
    drop(store);

    let store = Store::open(tmp.path()).unwrap();
    assert_eq!(store.subscribe_times(subscribe.id).unwrap(), inserted);
    // Seed a known earlier update time so this test needs no wall-clock wait.
    let db = Connection::open(tmp.path().join("subscribe.db")).unwrap();
    db.execute(
        "UPDATE subscribes SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![before, subscribe.id.to_string()],
    )
    .unwrap();
    subscribe.tracking_state = "paused".into();
    store.update_subscribe(&subscribe).unwrap();
    let updated = store.subscribe_times(subscribe.id).unwrap();
    assert_eq!(updated.0, inserted.0, "creation time must be preserved");
    assert!(updated.1.as_str() > before);
    assert_eq!(store.get_subscribe(subscribe.id).unwrap(), Some(subscribe));
}

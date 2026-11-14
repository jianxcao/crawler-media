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
        (", created_at", ", 1793059200000"),
        (", updated_at", ", 1793059200000"),
    ] {
        let error = db
            .execute(
                &format!(
                    "INSERT INTO subscribes
                     (id, user_id, media_id, coverage_kind, fetch_mode, filter_id, jobs_sync_generation, lifecycle_epoch{columns})
                     VALUES ('missing', 'user', 'media', 'movie', 'search', 'filter', 'generation', 1{values})"
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
    let before = 946_684_800_000_i64;
    for timestamp in [&inserted.0, &inserted.1] {
        assert!(
            *timestamp > before,
            "timestamp must be populated: {timestamp}"
        );
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
    assert!(updated.1 > before);
    assert_eq!(store.get_subscribe(subscribe.id).unwrap(), Some(subscribe));
}

#[test]
fn subscribe_creation_persists_one_integer_millisecond_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let clock = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    };
    let before = clock();
    let subscribe = insert_subscribe(&store);
    let after = clock();
    let db = Connection::open(tmp.path().join("subscribe.db")).unwrap();
    let pair: (rusqlite::types::Value, rusqlite::types::Value) = db
        .query_row(
            "SELECT created_at,updated_at FROM subscribes WHERE id=?1",
            [subscribe.id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(
        matches!(pair.0, rusqlite::types::Value::Integer(value) if (before..=after).contains(&value)),
        "{pair:?}"
    );
    assert_eq!(pair.0, pair.1);
}

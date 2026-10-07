use api::Store;

#[test]
fn legacy_device_primary_key_migrates_to_user_scoped_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("subscribe.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE playback_sessions (
            device_id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            season INTEGER,
            episode INTEGER,
            client TEXT,
            device_name TEXT,
            client_version TEXT,
            play_method TEXT NOT NULL DEFAULT 'local',
            position_ms INTEGER NOT NULL DEFAULT 0,
            start_position_ms INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER,
            paused INTEGER NOT NULL DEFAULT 0,
            watched_ms INTEGER NOT NULL DEFAULT 0,
            rate_bps INTEGER NOT NULL DEFAULT 0,
            bytes_sent INTEGER NOT NULL DEFAULT 0,
            connections INTEGER NOT NULL DEFAULT 0,
            admin_ended INTEGER NOT NULL DEFAULT 0,
            started_at INTEGER NOT NULL,
            last_report_at INTEGER NOT NULL
        );
        INSERT INTO playback_sessions (
            device_id, user_id, media_id, started_at, last_report_at
        ) VALUES (
            'living-room',
            '00000000-0000-0000-0000-000000000001',
            '00000000-0000-0000-0000-000000000002',
            1,
            1
        );",
    )
    .unwrap();
    drop(db);

    drop(Store::open(dir.path()).unwrap());

    let db = rusqlite::Connection::open(path).unwrap();
    let mut primary_key: Vec<(i64, String)> = db
        .prepare("PRAGMA table_info(playback_sessions)")
        .unwrap()
        .query_map([], |row| Ok((row.get(5)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .filter(|(position, _)| *position > 0)
        .collect();
    primary_key.sort_by_key(|(position, _)| *position);
    assert_eq!(
        primary_key,
        vec![(1, "user_id".into()), (2, "device_id".into())]
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM playback_sessions", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
}

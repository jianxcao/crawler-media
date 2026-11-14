use domain::{Media, MediaId, MediaKind, UnitKey, UserId};
use rusqlite::{Connection, params};
use store::{Store, stats::StatsScope};

#[test]
fn unrelated_episode_states_do_not_expand_batch_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let media = MediaId::new();
    let user = UserId::new();
    store
        .insert_media(&Media {
            id: media,
            kind: MediaKind::Tv,
            title: "Series".into(),
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
    let db = Connection::open(dir.path().join("library.db")).unwrap();
    let insert = |start: i32, end: i32| {
        let tx = db.unchecked_transaction().unwrap();
        for episode in start..end {
            tx.execute("INSERT INTO playback_units(user_id,media_id,season,episode,played,has_played_assertion,played_changed_at_ms) VALUES (?1,?2,2,?3,1,1,1)", params![user.to_string(), media.to_string(), episode]).unwrap();
        }
        tx.commit().unwrap();
    };
    let measure = || {
        store.with_stats(StatsScope::Library, || {
            store
                .effective_unit_states(user, media, &[UnitKey::new(1, 1), UnitKey::new(1, 2)])
                .unwrap()
        })
    };
    insert(0, 100);
    let (small, before) = measure();
    insert(100, 10_000);
    let (large, after) = measure();
    assert_eq!(small, large);
    println!("100: {before:?}; 10000: {after:?}");
    assert!(
        after.vm_steps <= before.vm_steps + 200,
        "unrelated units expanded work"
    );
    assert!(
        after.sql_rows <= before.sql_rows + 2,
        "unrelated units were decoded"
    );
}

#[test]
fn large_unit_request_uses_bounded_windows_and_preserves_assertions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let media = MediaId::new();
    let user = UserId::new();
    store
        .insert_media(&Media {
            id: media,
            kind: MediaKind::Tv,
            title: "Series".into(),
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
    let db = Connection::open(dir.path().join("library.db")).unwrap();
    db.execute("INSERT INTO playback_units(user_id,media_id,season,episode,played,has_played_assertion,played_changed_at_ms) VALUES (?1,?2,1,-1,1,1,10)", params![user.to_string(), media.to_string()]).unwrap();
    db.execute("INSERT INTO playback_units(user_id,media_id,season,episode,played,has_played_assertion,played_changed_at_ms) VALUES (?1,?2,1,501,0,1,10)", params![user.to_string(), media.to_string()]).unwrap();
    let units: Vec<_> = (0..1_000).map(|episode| UnitKey::new(1, episode)).collect();
    let (_, single) = store.with_stats(StatsScope::Library, || {
        store
            .effective_unit_states(user, media, &units[..1])
            .unwrap()
    });
    let (states, work) = store.with_stats(StatsScope::Library, || {
        store.effective_unit_states(user, media, &units).unwrap()
    });
    assert_eq!(states.len(), 1_000);
    assert!(states[0].as_ref().unwrap().played);
    assert!(!states[501].as_ref().unwrap().played);
    assert!(states[999].as_ref().unwrap().played);
    // Both calls include the same lease validation and snapshot bookkeeping.
    // The one-key call performs one business query; keep the existing allowance
    // of at most six business statements across all bounded windows.
    assert!(
        work.statements <= single.statements + 5,
        "one query per fixed key window: single={single:?}, batch={work:?}"
    );
}

#[test]
fn concurrent_write_between_windows_keeps_the_initial_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let media = MediaId::new();
    let user = UserId::new();
    store
        .insert_media(&Media {
            id: media,
            kind: MediaKind::Tv,
            title: "Series".into(),
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
    let db_path = dir.path().join("library.db");
    let db = Connection::open(&db_path).unwrap();
    for episode in [1, 800] {
        db.execute("INSERT INTO playback_units(user_id,media_id,season,episode,played,has_played_assertion,played_changed_at_ms) VALUES (?1,?2,1,?3,0,1,1)", params![user.to_string(), media.to_string(), episode]).unwrap();
    }
    drop(db);
    let (start_tx, start_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<bool>(1);
    let writer = std::thread::spawn(move || {
        if start_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .is_err()
        {
            return false;
        }
        let db = Connection::open(db_path).unwrap();
        db.busy_timeout(std::time::Duration::from_secs(3)).unwrap();
        let result = db.execute("UPDATE playback_units SET played=1, played_changed_at_ms=2 WHERE user_id=?1 AND media_id=?2", params![user.to_string(), media.to_string()]);
        let ok = result.is_ok();
        done_tx.send(ok).unwrap();
        ok
    });
    let mut triggered = false;
    let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let callback_observed = observed.clone();
    let units: Vec<_> = (0..1_000).map(|episode| UnitKey::new(1, episode)).collect();
    let states = store.with_library_progress(
        5_000,
        move || {
            if !triggered {
                triggered = true;
                if start_tx.send(()).is_ok()
                    && done_rx.recv_timeout(std::time::Duration::from_secs(4)) == Ok(true)
                {
                    callback_observed.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            false
        },
        || store.effective_unit_states(user, media, &units).unwrap(),
    );
    assert!(writer.join().unwrap());
    assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!states[1].as_ref().unwrap().played);
    assert!(!states[800].as_ref().unwrap().played);
    assert!(
        store
            .effective_unit_state(user, media, units[800])
            .unwrap()
            .unwrap()
            .played
    );
}

#[test]
fn season_coverage_work_ignores_other_season_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let media = MediaId::new();
    store
        .insert_media(&Media {
            id: media,
            kind: MediaKind::Tv,
            title: "Series".into(),
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
    let library: domain::LibraryId = store
        .default_library(MediaKind::Tv)
        .unwrap()
        .unwrap()
        .id
        .parse()
        .unwrap();
    let db = Connection::open(dir.path().join("library.db")).unwrap();
    let root: String = db
        .query_row(
            "SELECT root_id FROM library_roots WHERE library_id=?1 LIMIT 1",
            [library.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let insert = |season: i32, start: i32, end: i32| {
        let tx = db.unchecked_transaction().unwrap();
        for episode in start..end {
            let file = domain::LedgerId::new().to_string();
            let path = format!("/fixture/{season}/{episode}");
            tx.execute("INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,presence_state,publication_state,file_revision,source_version,quality_source,confidence) VALUES (?1,?2,?3,?4,'tv',?5,?5,'present','published',1,'v','release','high')", params![file, library.to_string(), root, media.to_string(), path]).unwrap();
            tx.execute("INSERT INTO library_file_units(file_id,library_id,media_id,season,episode) VALUES (?1,?2,?3,?4,?5)", params![file, library.to_string(), media.to_string(), season, episode]).unwrap();
        }
        tx.commit().unwrap();
    };
    insert(1, 1, 3);
    insert(2, 0, 100);
    db.execute_batch("ANALYZE").unwrap();
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    let (small, before) = store.with_stats(StatsScope::Library, || {
        store
            .file_coverage_in_library_season(library, media, 1)
            .unwrap()
    });
    insert(2, 100, 10_000);
    db.execute_batch("ANALYZE").unwrap();
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    let (large, after) = store.with_stats(StatsScope::Library, || {
        store
            .file_coverage_in_library_season(library, media, 1)
            .unwrap()
    });
    assert_eq!(small.len(), 2);
    assert_eq!(large.len(), 2);
    println!("coverage 100={before:?}; 10000={after:?}");
    assert!(
        after.vm_steps <= before.vm_steps + 200,
        "unrelated season files expanded work"
    );
    assert_eq!(after.sql_rows, before.sql_rows);
}

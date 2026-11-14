#![allow(dead_code)]
use domain::{LibraryId, Media, MediaId, MediaKind, UserId};
use rusqlite::params;
use store::Store;

pub(super) struct Fixture {
    pub tmp: tempfile::TempDir,
    pub store: Store,
    pub library: LibraryId,
    pub media: MediaId,
    pub user: UserId,
}

pub(super) fn fixture(episodes: usize) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            "Long Media",
            &["/integrity-long"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id == library.id)
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Long Media".into(),
        year: Some(2024),
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let user = UserId::new();
    let mut db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    let tx = db.transaction().unwrap();
    let file = domain::LedgerId::new().to_string();
    tx.execute(
        "INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,
          display_path,presence_state,publication_state,file_revision,source_version,
          quality_source,confidence) VALUES(?1,?2,?3,?4,'tv','/integrity-long/all.mkv',
          '/integrity-long/all.mkv','present','published',1,'v1','probe','high')",
        params![file, library.id, root.id, media.id.to_string()],
    )
    .unwrap();
    for episode in 1..=episodes {
        tx.execute(
            "INSERT INTO library_file_units(file_id,library_id,media_id,season,episode)
             VALUES(?1,?2,?3,1,?4)",
            params![file, library.id, media.id.to_string(), episode as i64],
        )
        .unwrap();
    }
    tx.execute(
        "INSERT INTO playback_units(user_id,media_id,season,episode,favorite,favorite_at)
         VALUES(?1,?2,-1,-1,1,50)",
        params![user.to_string(), media.id.to_string()],
    )
    .unwrap();
    tx.commit().unwrap();
    let violations: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    Fixture {
        tmp,
        store,
        library: library.id.parse().unwrap(),
        media: media.id,
        user,
    }
}

pub(super) fn finish(store: &Store, operation: &str) {
    for step in 1..30_000 {
        let status = store.advance_library_operation(operation, step, 1).unwrap();
        if status.state == "succeeded" {
            return;
        }
        assert_eq!(status.state, "applying", "{status:?}");
    }
    panic!("operation {operation} did not finish");
}

pub(super) fn add_pending_files(fixture: &Fixture, count: usize) {
    let mut db = rusqlite::Connection::open(fixture.tmp.path().join("library.db")).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    let tx = db.transaction().unwrap();
    for n in 0..count {
        tx.execute("INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,
            display_path,presence_state,publication_state,file_revision,source_version,quality_source,confidence)
            SELECT ?1,library_id,root_id,media_id,kind,?2,?2,'present','pending',1,'v1','probe','high'
            FROM library_files WHERE display_path='/integrity-long/all.mkv'",
            params![format!("00000000-0000-0000-0000-{n:012}"),format!("/integrity-long/pending-{n}.mkv")]).unwrap();
        tx.execute(
            "INSERT INTO library_file_units(file_id,library_id,media_id,season,episode)
            VALUES(?1,?2,?3,1,1)",
            params![
                format!("00000000-0000-0000-0000-{n:012}"),
                fixture.library.to_string(),
                fixture.media.to_string()
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

pub(super) fn many_derived_users(fixture: &Fixture, count: usize) {
    fixture
        .store
        .begin_library_integrity(fixture.library, true, "seed", 0)
        .unwrap();
    finish(&fixture.store, "seed");
    let mut db = rusqlite::Connection::open(fixture.tmp.path().join("library.db")).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    let tx = db.transaction().unwrap();
    for table in ["user_library_items", "user_library_continue_units"] {
        let names = tx
            .prepare(&format!("SELECT * FROM {table} LIMIT 0"))
            .unwrap()
            .column_names()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let select = names
            .iter()
            .map(|n| if n == "user_id" { "?1" } else { n.as_str() })
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "INSERT INTO {table}({}) SELECT {select} FROM {table} WHERE user_id=?2",
            names.join(",")
        );
        for n in 0..count {
            tx.execute(
                &sql,
                params![
                    format!("00000000-0000-0000-0000-{n:012}"),
                    fixture.user.to_string()
                ],
            )
            .unwrap();
        }
    }
    tx.execute("DELETE FROM playback_units", []).unwrap();
    tx.commit().unwrap();
}

pub(super) fn reach_reverse(fixture: &Fixture, operation: &str, table: u64, limit: usize) {
    for step in 0..limit {
        let view = fixture
            .store
            .library_operation_view(operation)
            .unwrap()
            .unwrap();
        if view.cursor.as_ref().is_some_and(|c| {
            c["current"]["phase"] == "Reverse" && c["current"]["table"].as_u64() == Some(table)
        }) {
            return;
        }
        let status = fixture
            .store
            .advance_library_operation(operation, step as i64, 1)
            .unwrap();
        assert_ne!(
            status.state, "succeeded",
            "did not reach requested reverse surface"
        );
    }
    panic!("did not reach requested reverse surface");
}

use domain::{LedgerId, LibraryId, Media, MediaId, MediaKind, UnitKey};
use rusqlite::{Connection, params};
use store::{Store, stats::StatsScope};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    db: Connection,
    library: LibraryId,
    media: MediaId,
    root: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let media = MediaId::new();
        store
            .insert_media(&Media {
                id: media,
                kind: MediaKind::Tv,
                title: "Coverage pages".into(),
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
        let library: LibraryId = store
            .default_library(MediaKind::Tv)
            .unwrap()
            .unwrap()
            .id
            .parse()
            .unwrap();
        let db = Connection::open(dir.path().join("library.db")).unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        let root = db
            .query_row(
                "SELECT root_id FROM library_roots WHERE library_id=?1 LIMIT 1",
                [library.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        Self {
            _dir: dir,
            store,
            db,
            library,
            media,
            root,
        }
    }

    fn seed(&self, start: u128, end: u128) {
        let tx = self.db.unchecked_transaction().unwrap();
        for index in start..end {
            let file = Self::file_id(index).to_string();
            let path = format!("/fixture/{index}.mkv");
            tx.execute("INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,presence_state,publication_state,file_revision,source_version,quality_source,confidence) VALUES (?1,?2,?3,?4,'tv',?5,?5,'present','published',1,'v','release','high')", params![file, self.library.to_string(), self.root, self.media.to_string(), path]).unwrap();
            tx.execute("INSERT INTO library_file_units(file_id,library_id,media_id,season,episode) VALUES (?1,?2,?3,1,?4)", params![file, self.library.to_string(), self.media.to_string(), index as i64]).unwrap();
        }
        tx.commit().unwrap();
    }

    fn file_id(index: u128) -> LedgerId {
        uuid::Uuid::from_u128(index).to_string().parse().unwrap()
    }
}

#[test]
fn fixed_file_page_does_not_read_other_files_coverage() {
    let fx = Fixture::new();
    fx.seed(1, 101);
    let page = fx
        .store
        .library_file_page(fx.library, fx.media, None, 50)
        .unwrap();
    let measure = || {
        fx.store.with_stats(StatsScope::Library, || {
            fx.store
                .file_units(fx.library, fx.media, &page.files)
                .unwrap()
        })
    };
    let (before, small) = measure();
    fx.seed(101, 10_001);
    let (unanalysed, large_fresh) = measure();
    assert_eq!(before, unanalysed);
    println!("file page coverage before ANALYZE: {large_fresh:?}");
    assert!(
        large_fresh.vm_steps <= small.vm_steps + 200,
        "unrelated files expanded query work before ANALYZE"
    );
    fx.db.execute_batch("ANALYZE").unwrap();
    // Load the new schema statistics outside the measured request.
    fx.store
        .file_units(fx.library, fx.media, &page.files)
        .unwrap();
    let (after, large) = measure();
    assert_eq!(before, after);
    assert_eq!(after.len(), 50);
    assert!(after.iter().all(|(_, units)| units.len() == 1));
    println!("file page coverage 100={small:?}; 10000={large:?}");
    let (_, lease) = fx.store.with_stats(StatsScope::Library, || {
        fx.store.file_units(fx.library, fx.media, &[]).unwrap()
    });
    assert_eq!(small.sql_rows - lease.sql_rows, 50);
    assert_eq!(
        large.sql_rows - lease.sql_rows,
        50,
        "decoded coverage outside the requested page"
    );
    assert!(
        large.vm_steps <= small.vm_steps + 200,
        "unrelated files expanded query work"
    );
    assert_eq!(large.fullscan_steps, 0);
}

#[test]
fn file_coverage_preserves_order_units_scope_and_empty_pages() {
    let fx = Fixture::new();
    fx.seed(1, 4);
    let id = Fixture::file_id(1);
    for unit in [UnitKey::new(0, 0), UnitKey::new(1, 2), UnitKey::WHOLE] {
        fx.db.execute("INSERT INTO library_file_units(file_id,library_id,media_id,season,episode) VALUES (?1,?2,?3,?4,?5)", params![id.to_string(), fx.library.to_string(), fx.media.to_string(), unit.season, unit.episode]).unwrap();
    }
    let mut page = fx
        .store
        .library_file_page(fx.library, fx.media, None, 50)
        .unwrap();
    page.files.reverse();
    fx.db
        .execute(
            "DELETE FROM library_file_units WHERE file_id=?1",
            [Fixture::file_id(2).to_string()],
        )
        .unwrap();
    let actual = fx
        .store
        .file_units(fx.library, fx.media, &page.files)
        .unwrap();
    assert_eq!(
        actual,
        vec![
            (Fixture::file_id(3), vec![UnitKey::new(1, 3)]),
            (
                id,
                vec![
                    UnitKey::WHOLE,
                    UnitKey::new(0, 0),
                    UnitKey::new(1, 1),
                    UnitKey::new(1, 2)
                ]
            ),
        ]
    );
    assert!(
        fx.store
            .file_units(LibraryId::new(), fx.media, &page.files)
            .unwrap()
            .is_empty()
    );
    assert!(
        fx.store
            .file_units(fx.library, MediaId::new(), &page.files)
            .unwrap()
            .is_empty()
    );
    let (empty, work) = fx.store.with_stats(StatsScope::Library, || {
        fx.store.file_units(fx.library, fx.media, &[]).unwrap()
    });
    assert!(empty.is_empty());
    let (_, lease) = fx.store.with_stats(StatsScope::Library, || {
        fx.store.with_read(store::Db::Library, |_| Ok(())).unwrap()
    });
    assert_eq!(
        work.statements, lease.statements,
        "empty pages must only validate their read lease"
    );
}

#[test]
fn coverage_batches_preserve_every_requested_file() {
    let fx = Fixture::new();
    fx.seed(1, 502);
    let mut files = Vec::new();
    let mut after = None;
    loop {
        let page = fx
            .store
            .library_file_page(fx.library, fx.media, after.as_deref(), 100)
            .unwrap();
        files.extend(page.files);
        after = page.next_cursor;
        if after.is_none() {
            break;
        }
    }
    let (coverage, work) = fx.store.with_stats(StatsScope::Library, || {
        fx.store.file_units(fx.library, fx.media, &files).unwrap()
    });
    assert_eq!(coverage.len(), 501);
    for (file, (id, units)) in files.iter().zip(&coverage) {
        assert_eq!(file.file_id, *id);
        assert_eq!(units.len(), 1);
    }
    let (_, lease) = fx.store.with_stats(StatsScope::Library, || {
        fx.store.file_units(fx.library, fx.media, &[]).unwrap()
    });
    assert_eq!(work.sql_rows - lease.sql_rows, 501);
    assert!(
        work.statements <= lease.statements + 4,
        "one query per bounded file window in one snapshot: {work:?}"
    );
}

use std::collections::BTreeSet;

use domain::{LedgerId, LibraryId, Media, MediaId, MediaKind, UnitKey};
use store::Store;
use store::stats::StatsScope;
use store::test_support::{FixtureFile, publish_fixture_file_units};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    library: LibraryId,
    expected: Vec<LedgerId>,
    packed: LedgerId,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Coverage".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let root = dir.path().join("target");
        std::fs::create_dir_all(&root).unwrap();
        let library = store
            .create_library(
                MediaKind::Tv,
                "target",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
            .parse()
            .unwrap();
        let mut expected = Vec::new();
        let packed = file_id(200);
        for index in 1..=225 {
            let units = if index == 200 {
                (1..=8).map(|episode| UnitKey::new(1, episode)).collect()
            } else {
                vec![UnitKey::new(2, index as i32)]
            };
            let file = publish_fixture_file_units(
                &store,
                &FixtureFile {
                    media_id: media.id,
                    file_id: Some(file_id(index)),
                    path: root.join(format!("{index:04}.mkv")).display().to_string(),
                    ..Default::default()
                },
                &units,
            )
            .unwrap();
            expected.push(file.file_id);
        }
        // Same Media in another Library must not leak into either page.
        let sibling = dir.path().join("sibling");
        std::fs::create_dir_all(&sibling).unwrap();
        let sibling_library = store
            .create_library(
                MediaKind::Tv,
                "sibling",
                &[sibling.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        publish_fixture_file_units(
            &store,
            &FixtureFile {
                media_id: media.id,
                file_id: Some(file_id(1_000)),
                path: sibling.join("Sibling.mkv").display().to_string(),
                ..Default::default()
            },
            &[UnitKey::new(1, 1)],
        )
        .unwrap();
        store.begin_library_removal(&sibling_library.id).unwrap();
        Self {
            _dir: dir,
            store,
            library,
            expected,
            packed,
        }
    }
}

fn file_id(index: u128) -> LedgerId {
    LedgerId::from_uuid(uuid::Uuid::from_u128(index))
}

#[test]
fn file_coverage_page_caps_files_and_preserves_the_last_files_complete_coverage() {
    let fix = Fixture::new();
    let page = fix
        .store
        .list_file_coverage_page(fix.library, None, usize::MAX)
        .unwrap();
    assert_eq!(
        page.len(),
        200,
        "the cap counts files, not joined unit rows"
    );
    assert_eq!(
        page.iter().map(|row| row.file.file_id).collect::<Vec<_>>(),
        fix.expected[..200]
    );
    assert!(page.iter().all(|row| row.file.library_id == fix.library));
    let packed = page
        .iter()
        .find(|row| row.file.file_id == fix.packed)
        .unwrap();
    assert_eq!(
        packed.units,
        (1..=8)
            .map(|episode| UnitKey::new(1, episode))
            .collect::<Vec<_>>()
    );
    let tail = fix
        .store
        .list_file_coverage_page(fix.library, Some(fix.packed), 200)
        .unwrap();
    assert_eq!(
        tail.iter().map(|row| row.file.file_id).collect::<Vec<_>>(),
        fix.expected[200..]
    );
}

#[test]
fn file_coverage_page_keysets_are_disjoint_complete_and_respect_small_limits() {
    let fix = Fixture::new();
    let mut after = None;
    let mut seen = BTreeSet::new();
    let mut actual = Vec::new();
    loop {
        let page = fix
            .store
            .list_file_coverage_page(fix.library, after, 7)
            .unwrap();
        assert!(page.len() <= 7);
        if page.is_empty() {
            break;
        }
        for row in &page {
            let id = row.file.file_id;
            assert!(after.is_none_or(|cursor| id.to_string() > cursor.to_string()));
            assert!(
                seen.insert(id.to_string()),
                "file repeated across keyset pages"
            );
            actual.push(id);
            assert_eq!(row.file.library_id, fix.library);
            if id == fix.packed {
                assert_eq!(row.units.len(), 8);
            }
        }
        after = page.last().map(|row| row.file.file_id);
    }
    assert_eq!(actual, fix.expected);
}

#[test]
fn file_coverage_page_sql_work_stays_bounded_by_the_requested_files() {
    let fix = Fixture::new();
    let (page, stats) = fix.store.with_stats(StatsScope::Library, || {
        fix.store
            .list_file_coverage_page(fix.library, Some(file_id(199)), 1)
            .unwrap()
    });
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].file.file_id, fix.packed);
    assert_eq!(page[0].units.len(), 8);
    // This is actual executed work. A small materialized page may itself be
    // scanned, so do not demand FULLSCAN_STEP=0 for every CTE in the query.
    let (_, lease) = fix.store.with_stats(StatsScope::Library, || {
        fix.store.with_read(store::Db::Library, |_| Ok(())).unwrap()
    });
    assert!(
        stats.sql_rows - lease.sql_rows <= 10,
        "bounded hydration: {stats:?}"
    );
    assert!(
        stats.fullscan_steps <= 10,
        "one packed file must not scan the Library: {stats:?}"
    );
    assert!(
        stats.vm_steps <= 5_000,
        "one packed file must not enumerate the Library: {stats:?}"
    );
}

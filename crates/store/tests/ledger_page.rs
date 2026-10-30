//! Complete admin ledger uses one joined fact read, never per-file lookup loops.
use domain::{Media, MediaId, MediaKind};
use store::test_support::{FixtureFile, publish_fixture_file};
use store::{Store, stats::StatsScope};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    root: String,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let root = dir.path().join("library").to_string_lossy().into_owned();
        store
            .create_library(MediaKind::Movie, "Ledger", &[&root], "everyone", true, &[])
            .unwrap();
        Self {
            _dir: dir,
            store,
            root,
        }
    }
    fn seed(&self, from: usize, to: usize) {
        for i in from..to {
            let media = Media {
                id: MediaId::new(),
                kind: MediaKind::Movie,
                title: format!("Ledger {i}"),
                year: Some(2024),
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            };
            self.store.insert_media(&media).unwrap();
            publish_fixture_file(
                &self.store,
                &FixtureFile {
                    media_id: media.id,
                    path: format!("{}/{i}.mkv", self.root),
                    transfer_mode: Some("copy".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        }
    }
}

#[test]
fn ledger_statement_count_is_constant() {
    let fixture = Fixture::new();
    fixture.seed(0, 10);
    let (small, small_work) = fixture
        .store
        .with_stats(StatsScope::Library, || fixture.store.ledger_rows().unwrap());
    fixture.seed(10, 200);
    let (large, large_work) = fixture
        .store
        .with_stats(StatsScope::Library, || fixture.store.ledger_rows().unwrap());
    assert!(small_work.statements > 0 && large_work.statements > 0);
    assert!(small_work.reader_checkouts > 0 && large_work.reader_checkouts > 0);
    assert_eq!(small.len(), 10);
    assert_eq!(large.len(), 200);
    assert_eq!(
        large_work.statements, small_work.statements,
        "per-file facts must be joined rather than queried once per row"
    );
    assert!(
        large_work.reader_checkouts <= 1,
        "complete ledger facts share one snapshot"
    );
    for row in &large {
        assert_eq!(row.media_kind, MediaKind::Movie);
        assert!(row.media_title.starts_with("Ledger "));
        assert_eq!(row.coverage.file.transfer_mode.as_deref(), Some("copy"));
    }
}

#[test]
fn ledger_fact_lookups_use_indexes() {
    let fixture = Fixture::new();
    fixture.seed(0, 200);
    let details = fixture.store.ledger_rows_query_plan().unwrap();
    // The complete response necessarily enumerates f. Fact JOINs must SEARCH
    // their PK/covering indexes, regardless of aliased table names.
    let mut joins = 0;
    for detail in &details {
        let words: Vec<_> = detail.split_whitespace().collect();
        if words.first() == Some(&"SCAN")
            && matches!(
                words.get(1),
                Some(&"m" | &"u" | &"l" | &"media" | &"library_file_units" | &"libraries")
            )
        {
            panic!("unbounded fact lookup: {detail}");
        }
        if words.first() == Some(&"SEARCH")
            && matches!(
                words.get(1),
                Some(&"m" | &"u" | &"media" | &"library_file_units")
            )
        {
            joins += 1;
        }
    }
    assert!(
        joins >= 2,
        "media and coverage lookup indexes are required: {details:?}"
    );
}

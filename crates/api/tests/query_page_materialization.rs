//! Direct selector behavior that HTTP normalizes away, plus selected fact stats.

use std::{path::PathBuf, sync::Arc};

use api::{ApiState, Store};
use domain::{
    Confidence, LibraryId, Media, MediaId, MediaKind, ProtocolItemQuery, ProtocolItemType,
    ProtocolParent, QualitySource, UserId,
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    state: ApiState,
    root: PathBuf,
    library: LibraryId,
    media: MediaId,
    user: UserId,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let state = ApiState::new(
            Store::open(dir.path().join("data")).unwrap(),
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        let root = dir.path().join("tv");
        std::fs::create_dir_all(&root).unwrap();
        let store = state.store();
        let library = store
            .create_library(
                MediaKind::Tv,
                "Shows",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
            .parse()
            .unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "One".into(),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let path = root.join("one.mkv");
        std::fs::write(&path, b"fixture").unwrap();
        store::test_support::publish_fixture_file(
            &store,
            &store::test_support::FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                season: Some(1),
                episode: Some(1),
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                ..Default::default()
            },
        )
        .unwrap();
        let user = store
            .list_users()
            .unwrap()
            .into_iter()
            .find(|user| store.user_role(user.id).unwrap() == "admin")
            .unwrap()
            .id;
        Self {
            dir,
            state,
            root,
            library,
            media: media.id,
            user,
        }
    }

    fn query(&self, limit: usize) -> ProtocolItemQuery {
        ProtocolItemQuery {
            user_id: self.user,
            parent: ProtocolParent::Library(self.library),
            series_id: None,
            season: None,
            person_ids: Vec::new(),
            studio_ids: Vec::new(),
            search_term: None,
            genres: Vec::new(),
            genre_ids: Vec::new(),
            filters: Vec::new(),
            is_favorite: None,
            is_played: None,
            include_types: vec![ProtocolItemType::Series],
            recursive: true,
            is_folder: None,
            sort: Vec::new(),
            offset: 0,
            limit,
        }
    }
}

#[test]
fn zero_limit_returns_exact_total_without_materializing_references() {
    let fx = Fixture::new();
    let store = fx.state.store();
    let query = fx.query(0);
    let page = store.protocol_item_page(&query).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(
        page.items.len(),
        query
            .limit
            .min((page.total as usize).saturating_sub(query.offset))
    );
    assert_eq!(
        store
            .protocol_page_inputs(fx.user, &page.items)
            .unwrap()
            .hydrated_rows(),
        0
    );
}

#[test]
fn instrumented_capture_counts_sql_and_series_date_ignores_uncovered_file() {
    let fx = Fixture::new();
    let store = fx.state.store();
    let selected = store.protocol_item_page(&fx.query(1)).unwrap();
    let (facts, stats) = store
        .protocol_page_inputs_with_stats(fx.user, &selected.items)
        .unwrap();
    assert_eq!(facts.hydrated_rows(), stats.hydrated_rows);
    assert!(stats.hydrated_rows >= 3);
    assert!(
        stats.vm_steps > 0 && stats.sql_rows > 0 && stats.statements > 0,
        "authorization and fact SQL must be observed: {stats:?}"
    );

    let source = selected.items[0].preferred_source.as_ref().unwrap();
    let conn = rusqlite::Connection::open(fx.dir.path().join("data/library.db")).unwrap();
    conn.execute(
        "UPDATE library_files SET created_ns=100 WHERE file_id=?1",
        [source.file_id.to_string()],
    )
    .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id == fx.library.to_string())
        .unwrap()
        .id;
    conn.execute(
        "INSERT INTO library_files (file_id,library_id,root_id,media_id,kind,path_key,display_path,
          presence_state,publication_state,file_revision,source_version,created_ns,quality_source,confidence)
         VALUES (?1,?2,?3,?4,'tv',?5,?5,'present','published',1,'orphan',1000,'probe','high')",
        rusqlite::params![domain::LedgerId::new().to_string(), fx.library.to_string(), root_id.to_string(),
            fx.media.to_string(), fx.root.join("uncovered.mkv").display().to_string()],
    ).unwrap();
    let scalar = store
        .protocol_series_scalar(fx.user, fx.media, &ProtocolParent::Library(fx.library))
        .unwrap();
    assert_eq!(scalar.episodes, 1);
    assert_eq!(scalar.latest_created_ns, Some(100));
}

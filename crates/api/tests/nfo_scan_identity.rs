use store::test_support::{publish_fixture_file, FixtureFile};
#[path = "management/common.rs"]
mod common;
use domain::{Media, MediaId, MediaKind};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::{collections::HashMap, sync::Arc};
use store::Store;

#[derive(Default)]
struct ScanCatalog {
    requests: Mutex<Vec<String>>,
    hit: Option<Media>,
}
impl api::catalog::Catalog for ScanCatalog {
    fn search_movie(&self, query: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.requests.lock().push(format!("search:{query}"));
        Ok(self
            .hit
            .clone()
            .into_iter()
            .map(|media| media::CatalogHit {
                media,
                poster_path: None,
                backdrop_path: None,
                rating: None,
                overview: None,
            })
            .collect())
    }
    fn search_tv(&self, query: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.search_movie(query)
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, _: MediaKind, id: &str) -> Result<Option<Media>, String> {
        self.requests.lock().push(format!("details:{id}"));
        Err("offline fixture".into())
    }
    fn metadata_with_preferences(
        &self,
        _: MediaKind,
        id: &str,
        _: &[String],
        _: &[String],
    ) -> Result<Option<media::ItemMeta>, String> {
        self.requests.lock().push(format!("metadata:{id}"));
        Err("offline fixture".into())
    }
}

#[tokio::test]
async fn public_scan_recovers_existing_media_from_local_work_alias() {
    let temp = tempfile::tempdir().unwrap();
    let state = common::state(
        temp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
    );
    let catalog = Arc::new(ScanCatalog::default());
    let state = state.with_catalog(catalog.clone());
    let shared = state.store();
    let (root, id, old) = {
        let store = shared.lock();
        let library = store
            .list_libraries()
            .unwrap()
            .into_iter()
            .find(|lib| lib.kind == MediaKind::Tv)
            .unwrap();
        let old = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "旧中文标题".into(),
            year: Some(2022),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("207890".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&old).unwrap();
        (library.root_paths[0].clone(), library.id, old)
    };
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("English.Name.2025.S01E03.strm");
    std::fs::write(&path, "https://invalid.example/fixture").unwrap();
    let nfo = "<tvshow><title>新中文标题</title><year>2022</year><tmdbid>207890</tmdbid></tvshow>";
    std::fs::write(root.join("tvshow.nfo"), nfo).unwrap();
    std::fs::write(root.join("poster.jpg"), b"local-poster").unwrap();
    std::fs::write(root.join("fanart.jpg"), b"local-fanart").unwrap();
    let app = api::router(state);
    common::scan_and_wait(&app, &id).await;
    let store = shared.lock();
    let row = store
        .file_coverage_by_path(&path.display().to_string())
        .unwrap()
        .unwrap();
    assert!(
        catalog.requests.lock().is_empty(),
        "complete local NFO must suppress catalog requests"
    );
    assert_eq!(
        std::fs::read(root.join("poster.jpg")).unwrap(),
        b"local-poster"
    );
    assert_eq!(
        std::fs::read(root.join("fanart.jpg")).unwrap(),
        b"local-fanart"
    );
    // probe worker 可能已经把这个 unit 跑完，所以断言"登记过 probe"而非"此刻仍在队列"。
    assert!(
        store
            .probe_unit_recorded_for_ledger(&row.file.file_id.to_string())
            .unwrap()
    );
    assert!(store.get_file_meta(&row.file.file_id.to_string()).unwrap().is_none());
    assert_eq!(row.file.media_id, old.id);
    assert_eq!(row.units, vec![domain::UnitKey::new(1, 3)]);
    assert_eq!(
        std::fs::read_to_string(root.join("tvshow.nfo")).unwrap(),
        nfo
    );
}

fn setup_rebind_movies(
    store: &Store,
    temp_path: &Path,
) -> (PathBuf, String, Media, Media, domain::LibraryFile) {
    let library = store
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|lib| lib.kind == MediaKind::Movie)
        .unwrap();
    let old = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "旧中文标题".into(),
        year: Some(2022),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("207890".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let mut provisional = old.clone();
    provisional.id = MediaId::new();
    provisional.title = "English Name".into();
    provisional.tmdb_id = None;
    store.insert_media(&old).unwrap();
    store.insert_media(&provisional).unwrap();
    // 台账行必须落在真实 Library 根内；放进另一个电影库，才能验证扫描不会
    // 跨库改写无关行。
    let other_root = temp_path.join("unrelated-library");
    std::fs::create_dir_all(&other_root).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Unrelated",
            &[other_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let spec = FixtureFile {
        file_id: Some(domain::LedgerId::new()),
        media_id: provisional.id,
        path: other_root
            .join("English.Name.2022.mkv")
            .display()
            .to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
        ..Default::default()
    };
    let unrelated = publish_fixture_file(&store, &spec).unwrap();
    (
        library.root_paths[0].clone(),
        library.id,
        old,
        provisional,
        unrelated,
    )
}

/// Adopted metadata only fills what the NFO input leaves missing, so it doubles as the
/// "lower layer" the card must fall back to once the input row is gone.
fn adopt_provider_overview(store: &Store, media_id: MediaId) {
    let mut fields = domain::MetadataFields::default();
    fields.title = domain::FieldValue::Value("Reconcile Movie".into());
    fields.overview = domain::FieldValue::Value("Provider".into());
    let adoption = domain::MetadataAdoption {
        media_id,
        scope: domain::EntityScope::Work,
        locale: "zh-CN".into(),
        source: "tmdb".into(),
        provider_entity_id: "1".into(),
        provider_work_id: "1".into(),
        fetched_at: 1000,
        normalized: domain::NormalizedMetadata {
            fields,
            field_origins: Default::default(),
        },
        expected_revision: None,
        preferences: domain::MetadataPreferences {
            language_priority: vec!["zh-CN".into()],
            cert_country_priority: vec!["CN".into()],
        },
    };
    store.adopt_metadata(&adoption, 1000).unwrap();
}

/// 已有文件的 sidecar 每次相关扫描都必须重新观察：改成 Y 显示 Y；解析不了或坐标不清
/// 保留上一份有效输入；sidecar 删除才解除输入、露出 adopted 的下层值。
#[tokio::test]
async fn existing_file_sidecar_changes_and_deletes_are_reconciled() {
    const LOCALE: &str = "zh-CN";
    let temp = tempfile::tempdir().unwrap();
    let state = common::state(
        temp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
    );
    let state = state.with_catalog(Arc::new(ScanCatalog::default()));
    let shared = state.store();
    let (root, library_id) = {
        let store = shared.lock();
        let library = store
            .list_libraries()
            .unwrap()
            .into_iter()
            .find(|lib| lib.kind == MediaKind::Movie)
            .unwrap();
        (library.root_paths[0].clone(), library.id)
    };
    std::fs::create_dir_all(&root).unwrap();
    let video = root.join("Reconcile.Movie.2020.strm");
    std::fs::write(&video, "https://invalid.example/fixture").unwrap();
    let sidecar = video.with_extension("nfo");
    let body =
        |plot: &str| format!("<movie><title>Reconcile Movie</title><plot>{plot}</plot></movie>");
    std::fs::write(&sidecar, body("X")).unwrap();
    let app = api::router(state);
    common::scan_and_wait(&app, &library_id).await;
    let media_id = {
        let store = shared.lock();
        store
            .file_coverage_by_path(&video.display().to_string())
            .unwrap()
            .unwrap()
            .file
            .media_id
    };
    let library: domain::LibraryId = library_id.parse().unwrap();
    let detail = |store: &Store| {
        store
            .library_item_detail(library, media_id, LOCALE)
            .unwrap()
            .unwrap()
    };
    assert_eq!(
        detail(&shared.lock())
            .metadata
            .overview
            .value()
            .map(String::as_str),
        Some("X"),
        "首次入库必须观察 sidecar"
    );
    adopt_provider_overview(&shared.lock(), media_id);

    std::fs::write(&sidecar, body("Y")).unwrap();
    common::scan_and_wait(&app, &library_id).await;
    let edited = detail(&shared.lock());
    assert_eq!(
        edited.metadata.overview.value().map(String::as_str),
        Some("Y")
    );

    // 无法读成 NFO 文档：不采纳也不删除，保留上一份有效输入。
    std::fs::write(&sidecar, "not xml at all").unwrap();
    common::scan_and_wait(&app, &library_id).await;
    let invalid = detail(&shared.lock());
    assert_eq!(invalid.metadata.overview, edited.metadata.overview);

    // 合法的 XML，但在电影库里是剧集坐标：无法确定实体层，同样不动已有输入。
    std::fs::write(&sidecar, "<episodedetails><plot>Z</plot></episodedetails>").unwrap();
    common::scan_and_wait(&app, &library_id).await;
    let unknown = detail(&shared.lock());
    assert_eq!(unknown.metadata.overview, edited.metadata.overview);

    std::fs::remove_file(&sidecar).unwrap();
    common::scan_and_wait(&app, &library_id).await;
    let deleted = detail(&shared.lock());
    assert_eq!(
        deleted.metadata.overview.value().map(String::as_str),
        Some("Provider")
    );
}

#[tokio::test]
async fn incomplete_nfo_online_alias_rebinds_only_new_scanned_rows() {
    let temp = tempfile::tempdir().unwrap();
    let state = common::state(
        temp.path(),
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
    );
    let shared = state.store();
    let (root, id, old, provisional, unrelated) = setup_rebind_movies(&shared.lock(), temp.path());
    let mut hit = old.clone();
    hit.title = "English Name".into();
    let catalog = Arc::new(ScanCatalog {
        hit: Some(hit),
        ..Default::default()
    });
    let state = state.with_catalog(catalog.clone());
    std::fs::create_dir_all(&root).unwrap();
    let paths: [PathBuf; 2] = [
        root.join("English.Name.2022.1080p.strm"),
        root.join("English.Name.2022.720p.strm"),
    ];
    for path in &paths {
        std::fs::write(path, "https://invalid.example/fixture").unwrap();
        std::fs::write(
            path.with_extension("nfo"),
            "<movie><title>English Name</title></movie>",
        )
        .unwrap();
    }
    let app = api::router(state);
    common::scan_and_wait(&app, &id).await;
    assert!(
        catalog
            .requests
            .lock()
            .iter()
            .any(|request| request.starts_with("search:"))
    );
    let store = shared.lock();
    for path in &paths {
        let row = store
            .file_coverage_by_path(&path.display().to_string())
            .unwrap()
            .unwrap();
        assert_eq!(row.file.media_id, old.id);
        // probe worker 可能在断言前就跑完这个 unit，所以断言"曾经排入过 probe"
        // 而不是"此刻仍在队列里"。
        assert!(
            store
                .probe_unit_recorded_for_ledger(&row.file.file_id.to_string())
                .unwrap(),
            "扫描必须为台账行 {} 登记 probe",
            row.file.file_id
        );
    }
    let untouched = store.file_coverage_by_path(&unrelated.path.display().to_string()).unwrap().unwrap();
    assert_eq!(untouched.file.file_id, unrelated.file_id);
    assert_eq!(untouched.file.media_id, provisional.id);
    assert!(
        store
            .get_media(provisional.id)
            .unwrap()
            .unwrap()
            .tmdb_id
            .is_none()
    );
}

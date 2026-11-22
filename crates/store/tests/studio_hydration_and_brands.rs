use domain::{
    EntityScope, LibraryId, Media, MediaId, MediaKind, MetadataAdoption, MetadataPreferences,
    UnitKey,
};
use store::Store;
use store::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::metadata::{ScrapedDisplay, StoredBrand};

fn publish_item(
    store: &Store,
    library_id: &str,
    kind: MediaKind,
    root: &std::path::Path,
    name: &str,
) -> Media {
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.library_id.as_str() == library_id)
        .unwrap()
        .id;
    let media = Media {
        id: MediaId::new(),
        kind,
        title: name.into(),
        year: Some(2024),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some(format!("tmdb-{name}")),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let video = root.join(format!("{name}.mkv"));
    std::fs::write(&video, b"bytes").unwrap();
    let unit = if kind == MediaKind::Tv {
        UnitKey::new(1, 1)
    } else {
        UnitKey::WHOLE
    };
    let file = domain::LibraryFile {
        file_id: domain::LedgerId::new(),
        library_id: library_id.parse().unwrap(),
        root_id,
        media_id: media.id,
        kind,
        path: video,
        version: domain::FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: domain::FilePresence::Present,
        publication: domain::FilePublication::Published,
        size_bytes: Some(5),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    let scope = WriteScope {
        library_id: library_id.parse().unwrap(),
        config_revision: 0,
    };
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file,
                units: &[unit],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1,
        )
        .unwrap();
    media
}

fn adopt_tmdb(store: &Store, media: &Media, raw_payload: &str) {
    let pref = MetadataPreferences {
        language_priority: vec!["zh-CN".into()],
        cert_country_priority: vec!["CN".into(), "US".into()],
    };
    let normalized =
        media::normalize_metadata("tmdb", EntityScope::Work, raw_payload, &pref).unwrap();
    let expected_revision = store.media_revision(media.id).unwrap();
    let tmdb_id = media.tmdb_id.clone().unwrap();
    let adoption = MetadataAdoption {
        media_id: media.id,
        scope: EntityScope::Work,
        locale: "zh-CN".into(),
        source: "tmdb".into(),
        provider_entity_id: tmdb_id.clone(),
        provider_work_id: tmdb_id,
        fetched_at: 1_000,
        normalized,
        expected_revision,
        preferences: pref,
    };
    store.adopt_metadata(&adoption, 1_000).unwrap();
}

#[test]
fn adopt_and_scraped_display_hydrate_nested_studio_and_logo_for_browse_page() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let root = tmp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let lib_id: LibraryId = library.id.parse().unwrap();
    let show = publish_item(&store, &library.id, MediaKind::Tv, &root, "ShowA");

    adopt_tmdb(
        &store,
        &show,
        r#"{
            "name": "Show A",
            "production_companies": [{"name": "New Classics Media"}]
        }"#,
    );

    let before_facts = store.library_item_facts(lib_id, &[show.id], None).unwrap();
    assert_eq!(
        before_facts[&show.id].studio.as_deref(),
        Some("New Classics Media")
    );

    store
        .record_scraped_display(
            show.id,
            "zh-CN",
            ScrapedDisplay {
                rating: Some(8.6),
                release_date: Some("2024-06-01"),
                studio: Some("CCTV-8"),
                studio_logo: Some("/2aXk02xBwG2Nhu4lftabfqSpwY3.png"),
                studio_logos: &[],
            },
        )
        .unwrap();

    let page = store
        .library_item_page(&LibraryQuery {
            library_id: lib_id,
            user_id: None,
            filter: BrowseFilter::default(),
            order: BrowseOrder::AddedAt,
            descending: true,
            page: BrowsePage::Cursor {
                after: None,
                limit: 20,
            },
            include_total: true,
        })
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].rating, Some(8.6));

    let facts = store.library_item_facts(lib_id, &[show.id], None).unwrap();
    assert_eq!(facts[&show.id].studio.as_deref(), Some("CCTV-8"));
    assert_eq!(
        facts[&show.id].studio_logo.as_deref(),
        Some("/2aXk02xBwG2Nhu4lftabfqSpwY3.png")
    );

    let detail = store
        .library_item_detail(lib_id, show.id, "")
        .unwrap()
        .unwrap();
    assert_eq!(
        detail.metadata.studios.value().cloned().unwrap_or_default(),
        vec!["CCTV-8".to_string(), "New Classics Media".to_string()],
        "record_scraped_display must prepend missing primary network without dropping companies"
    );
}

#[test]
fn studio_logos_persist_across_recomposition_and_detail_synthesizes_fallback_brands() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let lib_id: LibraryId = library.id.parse().unwrap();

    // 1. Multi-brand studio_logos survive write_effective re-composition and preserve multi-element studios.
    let movie = publish_item(&store, &library.id, MediaKind::Movie, &root, "Matrix");
    let raw = r#"{
        "title": "The Matrix",
        "production_companies": [
            {"name": "Warner Bros. Pictures"},
            {"name": "Village Roadshow Pictures"}
        ]
    }"#;
    adopt_tmdb(&store, &movie, raw);

    let stored_brands = [
        StoredBrand {
            name: "Warner Bros. Pictures".into(),
            logo_path: Some("/wb.png".into()),
            kind: "studio".into(),
        },
        StoredBrand {
            name: "Village Roadshow Pictures".into(),
            logo_path: Some("/vrp.png".into()),
            kind: "studio".into(),
        },
    ];
    store
        .record_scraped_display(
            movie.id,
            "zh-CN",
            ScrapedDisplay {
                rating: Some(8.7),
                release_date: Some("1999-03-30"),
                studio: Some("Warner Bros. Pictures"),
                studio_logo: Some("/wb.png"),
                studio_logos: &stored_brands,
            },
        )
        .unwrap();

    // Re-adopt metadata to trigger write_effective recomposition.
    adopt_tmdb(&store, &movie, raw);

    let detail = store
        .library_item_detail(lib_id, movie.id, "")
        .unwrap()
        .unwrap();
    assert_eq!(
        detail.metadata.studios.value().cloned().unwrap_or_default(),
        vec![
            "Warner Bros. Pictures".to_string(),
            "Village Roadshow Pictures".to_string()
        ]
    );
    assert_eq!(detail.brands, stored_brands);

    // 2. Zero-rescrape fallback for existing rows without studio_logos.
    let legacy = publish_item(&store, &library.id, MediaKind::Movie, &root, "LegacyRow");
    adopt_tmdb(
        &store,
        &legacy,
        r#"{
            "title": "Legacy Row",
            "production_companies": [
                {"name": "CCTV-8"},
                {"name": "Tencent Video"}
            ]
        }"#,
    );
    store
        .record_scraped_display(
            legacy.id,
            "zh-CN",
            ScrapedDisplay {
                rating: Some(8.1),
                release_date: Some("2023-01-01"),
                studio: Some("CCTV-8"),
                studio_logo: Some("/2aXk02xBwG2Nhu4lftabfqSpwY3.png"),
                studio_logos: &[],
            },
        )
        .unwrap();

    let legacy_detail = store
        .library_item_detail(lib_id, legacy.id, "")
        .unwrap()
        .unwrap();
    assert_eq!(
        legacy_detail.brands,
        vec![
            StoredBrand {
                name: "CCTV-8".into(),
                logo_path: Some("/2aXk02xBwG2Nhu4lftabfqSpwY3.png".into()),
                kind: "studio".into(),
            },
            StoredBrand {
                name: "Tencent Video".into(),
                logo_path: None,
                kind: "studio".into(),
            },
        ]
    );
}

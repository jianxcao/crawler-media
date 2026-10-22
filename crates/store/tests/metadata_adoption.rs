use domain::{EntityScope, MetadataAdoption, MetadataPreferences};
use store::Store;

#[test]
fn metadata_adoption_persists_source_snapshot_and_effective_row() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    let media_id = "00000000-0000-0000-0000-000000000001";
    let m = domain::Media {
        id: media_id.parse().unwrap(),
        kind: domain::MediaKind::Movie,
        title: "Test Movie".into(),
        year: Some(2023),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("100".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&m).unwrap();

    let raw_payload = r#"{
        "title": "Adopted Title",
        "overview": "Adopted Overview",
        "tagline": "Adopted Tagline",
        "release_dates": {
            "results": [
                {
                    "iso_3166_1": "US",
                    "release_dates": [{"certification": "R"}]
                }
            ]
        }
    }"#;
    let pref = MetadataPreferences {
        language_priority: vec!["en-US".into()],
        cert_country_priority: vec!["US".into()],
    };
    let normalized =
        media::normalize_metadata("tmdb", EntityScope::Work, raw_payload, &pref).unwrap();

    let adoption = MetadataAdoption {
        media_id: m.id,
        scope: EntityScope::Work,
        locale: "zh-CN".into(),
        source: "tmdb".into(),
        provider_entity_id: "100".into(),
        provider_work_id: "100".into(),
        fetched_at: 1000,
        normalized,
        expected_revision: None,
        preferences: pref,
    };

    let rev = store
        .adopt_metadata(&adoption, 1000)
        .expect("adopt_metadata");
    assert!(rev >= 1);

    // Read back effective metadata
    let effective = store
        .get_effective_metadata(m.id, "zh-CN")
        .unwrap()
        .expect("effective exists");
    assert_eq!(effective.title, "Adopted Title");
    assert_eq!(effective.overview.as_deref(), Some("Adopted Overview"));
}

#[test]
fn complete_source_metadata_survives_adoption_and_detail_read() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            domain::MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let media_id = domain::MediaId::new();
    let m = domain::Media {
        id: media_id,
        kind: domain::MediaKind::Movie,
        title: "Test Movie".into(),
        year: Some(2023),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("100".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&m).unwrap();

    let raw_payload = r#"{
        "title": "Complete Movie",
        "runtime": 100,
        "vote_average": 8.5,
        "vote_count": 1200,
        "release_dates": {
            "results": [
                {
                    "iso_3166_1": "US",
                    "release_dates": [{"certification": "R"}]
                }
            ]
        },
        "production_countries": [
            {"iso_3166_1": "US", "name": "United States"}
        ],
        "credits": {
            "cast": [
                {"name": "Actor A", "character": "Hero", "id": 123, "profile_path": "/thumb.jpg", "order": 0}
            ]
        }
    }"#;
    let pref = MetadataPreferences {
        language_priority: vec!["en-US".into()],
        cert_country_priority: vec!["US".into()],
    };
    let normalized =
        media::normalize_metadata("tmdb", EntityScope::Work, raw_payload, &pref).unwrap();

    let adoption = MetadataAdoption {
        media_id: m.id,
        scope: EntityScope::Work,
        locale: "zh-CN".into(),
        source: "tmdb".into(),
        provider_entity_id: "100".into(),
        provider_work_id: "100".into(),
        fetched_at: 1000,
        normalized,
        expected_revision: None,
        preferences: pref,
    };

    let rev = store
        .adopt_metadata(&adoption, 1000)
        .expect("adopt_metadata");
    assert!(rev >= 1);

    let composed = store
        .effective_metadata_fields(
            library.id.parse().unwrap(),
            m.id,
            EntityScope::Work,
            "zh-CN",
        )
        .expect("effective_metadata_fields");

    assert_eq!(composed.fields.runtime_minutes.value(), Some(&100));
    assert_eq!(composed.fields.rating.value(), Some(&8.5));
    assert_eq!(
        composed.fields.content_rating.value().map(String::as_str),
        Some("R")
    );
    assert_eq!(
        composed
            .field_origins
            .get("content_rating")
            .and_then(|o| o.selection.certification_country.as_deref()),
        Some("US")
    );
}

fn adoption(media: domain::MediaId, source: &str, overview: &str) -> MetadataAdoption {
    let mut fields = domain::MetadataFields::default();
    fields.title = domain::FieldValue::Value("Priority Title".into());
    fields.overview = domain::FieldValue::Value(overview.into());
    MetadataAdoption {
        media_id: media,
        scope: EntityScope::Work,
        locale: "zh-CN".into(),
        source: source.into(),
        provider_entity_id: "1".into(),
        provider_work_id: "1".into(),
        fetched_at: 1000,
        normalized: domain::NormalizedMetadata {
            fields,
            field_origins: Default::default(),
        },
        expected_revision: None,
        preferences: MetadataPreferences {
            language_priority: vec!["zh-CN".into()],
            cert_country_priority: vec!["CN".into()],
        },
    }
}

#[test]
fn lower_priority_source_does_not_overwrite_higher_priority_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Priority".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("1".into()),
        douban_id: Some("2".into()),
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();

    store
        .adopt_metadata(&adoption(media.id, "tmdb", "TMDB overview"), 1)
        .unwrap();
    store
        .adopt_metadata(&adoption(media.id, "douban", "Douban overview"), 1)
        .unwrap();

    let effective = store
        .get_effective_metadata(media.id, "zh-CN")
        .unwrap()
        .unwrap();
    assert_eq!(effective.overview.as_deref(), Some("TMDB overview"));

    // A higher-priority refresh still replaces the value.
    store
        .adopt_metadata(&adoption(media.id, "tmdb", "TMDB v2"), 2)
        .unwrap();
    let effective = store
        .get_effective_metadata(media.id, "zh-CN")
        .unwrap()
        .unwrap();
    assert_eq!(effective.overview.as_deref(), Some("TMDB v2"));
}

#[test]
fn invalid_nfo_keeps_the_last_valid_metadata_input() {
    use domain::{
        Confidence, FilePresence, FilePublication, FileVersion, LibraryFile, QualitySource, UnitKey,
    };
    use store::library_write::{FilePublication as StorePublication, WriteScope};

    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            domain::MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.library_id.as_deref() == Some(&library.id))
        .unwrap()
        .id;
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "NFO".into(),
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

    let video = root.join("Movie.mkv");
    std::fs::write(&video, b"bytes").unwrap();
    let nfo = video.with_extension("nfo");
    std::fs::write(&nfo, "<movie><plot>Keep me</plot></movie>").unwrap();
    let file = LibraryFile {
        file_id: domain::LedgerId::new(),
        library_id: library.id.parse().unwrap(),
        root_id,
        media_id: media.id,
        kind: domain::MediaKind::Movie,
        path: video,
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
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
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    let scope = WriteScope {
        library_id: library.id.parse().unwrap(),
        config_revision: 0,
    };
    let observe = |file: &LibraryFile| {
        store::metadata::observe_nfo_inputs(
            &file.path,
            file.kind,
            &[UnitKey::WHOLE],
            "",
            &file.version,
            scope.config_revision,
        )
    };
    let publish = |file: &LibraryFile| {
        let observations = observe(file);
        store
            .publish_files(
                &scope,
                &[StorePublication {
                    file,
                    units: &[UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: Some(&observations),
                    probe_required: false,
                }],
                1,
            )
            .unwrap();
    };
    publish(&file);
    let before = store
        .library_item_detail(scope.library_id, media.id, "")
        .unwrap()
        .unwrap()
        .metadata
        .overview
        .value()
        .cloned();
    assert_eq!(before.as_deref(), Some("Keep me"));

    std::fs::write(&nfo, "<movie><plot>broken").unwrap();
    publish(&file);
    let after = store
        .library_item_detail(scope.library_id, media.id, "")
        .unwrap()
        .unwrap()
        .metadata
        .overview
        .value()
        .cloned();
    assert_eq!(
        after, before,
        "an unparsable sidecar must not erase the last valid input"
    );
}

#[test]
fn record_scraped_display_with_studio_updates_and_allows_library_reduction() {
    use domain::{
        Confidence, FilePresence, FilePublication, FileVersion, LibraryFile, MediaKind,
        QualitySource, UnitKey,
    };
    use store::library_write::{FilePublication as StorePublication, WriteScope};
    use store::metadata::ScrapedDisplay;

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
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.library_id.as_deref() == Some(&library.id))
        .unwrap()
        .id;
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: MediaKind::Movie,
        title: "Studio Test".into(),
        year: Some(2024),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("12345".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();

    let video = root.join("Movie.mkv");
    std::fs::write(&video, b"bytes").unwrap();
    let file = LibraryFile {
        file_id: domain::LedgerId::new(),
        library_id: library.id.parse().unwrap(),
        root_id,
        media_id: media.id,
        kind: MediaKind::Movie,
        path: video,
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
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
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    let scope = WriteScope {
        library_id: library.id.parse().unwrap(),
        config_revision: 0,
    };
    store
        .publish_files(
            &scope,
            &[StorePublication {
                file: &file,
                units: &[UnitKey::WHOLE],
                expected_version: None,
                nfo_observation: None,
                probe_required: false,
            }],
            1,
        )
        .unwrap();

    // Now record scraped display with a studio name
    store
        .record_scraped_display(
            media.id,
            "zh-CN",
            ScrapedDisplay {
                rating: Some(8.5),
                release_date: Some("2024-01-01"),
                studio: Some("Studio X"),
                studio_logo: Some("https://example.com/logo.png"),
            },
        )
        .unwrap();

    // Trigger reduction / library item detail read which runs adopted_for_library
    let detail = store
        .library_item_detail(scope.library_id, media.id, "zh-CN")
        .unwrap()
        .expect("detail exists");

    let facts = store
        .adopted_filter_facts_for_media(&[media.id], Some("zh-CN"))
        .unwrap();
    let media_facts = facts.get(&media.id).expect("facts exist");
    assert_eq!(media_facts.studios, vec!["Studio X".to_string()]);
}

fn movie_with_library(store: &Store, root: &std::path::Path) -> (domain::LibraryId, domain::Media) {
    std::fs::create_dir_all(root).unwrap();
    let library = store
        .create_library(
            domain::MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Base".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: Some("1".into()),
        douban_id: Some("2".into()),
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    (library.id.parse().unwrap(), media)
}

/// R16: a successful TMDB refresh that no longer reports an overview stops contributing it;
/// the persisted effective row is recomposed from the remaining (Douban) snapshot and agrees
/// with the composer, value and origin alike.
#[test]
fn successful_missing_recomposes_from_remaining_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (library, media) = movie_with_library(&store, &tmp.path().join("m"));
    store
        .adopt_metadata(&adoption(media.id, "tmdb", "OLD WINNER"), 1)
        .unwrap();
    store
        .adopt_metadata(&adoption(media.id, "douban", "BACKUP"), 1)
        .unwrap();
    let mut v2 = adoption(media.id, "tmdb", "unused");
    v2.normalized.fields.overview = domain::FieldValue::Missing;
    store.adopt_metadata(&v2, 2).unwrap();

    let persisted = store
        .get_effective_metadata(media.id, "zh-CN")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.overview, Some("BACKUP".into()));
    let composed = store
        .effective_metadata_fields(library, media.id, EntityScope::Work, "zh-CN")
        .unwrap();
    assert_eq!(
        composed.fields.overview.value().map(String::as_str),
        Some("BACKUP")
    );
    assert_eq!(composed.field_origins["overview"].source, "douban");
    let persisted_origins: serde_json::Value =
        serde_json::from_str(&persisted.field_origins_json).unwrap();
    assert_eq!(persisted_origins["overview"]["source"], "douban");
}

/// Adoption order must not change the composed values or their origins.
#[test]
fn adoption_order_does_not_change_composition() {
    let compose = |order: [&str; 2]| {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let (library, media) = movie_with_library(&store, &tmp.path().join("m"));
        for source in order {
            store
                .adopt_metadata(&adoption(media.id, source, &format!("{source} plot")), 1)
                .unwrap();
        }
        let composed = store
            .effective_metadata_fields(library, media.id, EntityScope::Work, "zh-CN")
            .unwrap();
        let persisted = store
            .get_effective_metadata(media.id, "zh-CN")
            .unwrap()
            .unwrap();
        (
            composed.fields,
            persisted.overview,
            persisted
                .field_origins_json
                .replace(&media.id.to_string(), "M"),
        )
    };
    assert_eq!(compose(["tmdb", "douban"]), compose(["douban", "tmdb"]));
}

/// R15: a manual override is composed into the persisted effective row.
#[test]
fn override_reaches_effective_row_and_detail() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (library, media) = movie_with_library(&store, &tmp.path().join("m"));
    store
        .adopt_metadata(&adoption(media.id, "tmdb", "V1"), 1)
        .unwrap();
    let mut fields = domain::MetadataFields::default();
    fields.overview = domain::FieldValue::Value("MANUAL".into());
    store
        .set_metadata_overrides(media.id, EntityScope::Work, "zh-CN", &fields, 0)
        .unwrap();
    let persisted = store
        .get_effective_metadata(media.id, "zh-CN")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.overview, Some("MANUAL".into()));
    let composed = store
        .effective_metadata_fields(library, media.id, EntityScope::Work, "zh-CN")
        .unwrap();
    assert_eq!(composed.field_origins["overview"].source, "manual");

    // A later provider refresh does not displace the override.
    store
        .adopt_metadata(&adoption(media.id, "tmdb", "V2"), 2)
        .unwrap();
    let persisted = store
        .get_effective_metadata(media.id, "zh-CN")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.overview, Some("MANUAL".into()));
}

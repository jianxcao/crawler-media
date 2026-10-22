//! Library browse contract: SQL filters, keyset paging, facet counts, page facts and
//! detail/episode reads — all against the new `library_*` summaries.

use domain::{
    Confidence, EntityScope, FieldValue, FilePresence, FilePublication, FileVersion, LibraryFile,
    LibraryId, Media, MediaId, MediaKind, MetadataAdoption, MetadataFields, MetadataGenre,
    MetadataPreferences, NormalizedMetadata, QualitySource, UnitKey, UserId,
};
use store::library_query::{
    BrowseFilter, BrowseOrder, BrowsePage, FacetRequest, FacetTier, LibraryQuery,
};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use store::Store;

#[path = "library_query_filters/browse.rs"]
mod browse;
#[path = "library_query_filters/filters.rs"]
mod filters;

fn create_media(
    store: &Store,
    id: &str,
    title: &str,
    kind: MediaKind,
    year: Option<u16>,
    external_id: Option<&str>,
) -> Media {
    let media = Media {
        id: id.parse().unwrap(),
        kind,
        title: title.to_string(),
        year,
        original_title: None,
        tmdb_id: external_id.map(str::to_string),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: Vec::new(),
    };
    store.insert_media(&media).unwrap();
    media
}

fn create_library(store: &Store, name: &str, kind: MediaKind, root: &str) -> (store::Library, String) {
    let library = store
        .create_library(kind, name, &[root], "everyone", true, &[])
        .unwrap();
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.library_id == Some(library.id.clone()))
        .expect("library root");
    (library, root.id)
}

struct FileShape<'a> {
    file_id: &'a str,
    path: &'a str,
    resolution: Option<&'a str>,
    hdr: Option<&'a str>,
    presence: FilePresence,
}

fn make_file(
    lib: &store::Library,
    root_id: &str,
    media_id: MediaId,
    shape: FileShape<'_>,
) -> LibraryFile {
    LibraryFile {
        file_id: shape.file_id.parse().unwrap(),
        library_id: lib.id.parse().unwrap(),
        root_id: root_id.to_string(),
        media_id,
        kind: lib.kind,
        path: shape.path.into(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".to_string(),
        },
        presence: shape.presence,
        publication: FilePublication::Published,
        size_bytes: Some(1_000_000),
        stat_known: true,
        created_ns: Some(100),
        modified_ns: Some(200),
        source_digest: Some("hash".to_string()),
        source_path: None,
        transfer_mode: None,
        resolution: shape.resolution.map(str::to_string),
        codec: Some("hevc".to_string()),
        hdr: shape.hdr.map(str::to_string),
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: Some(120_000),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

fn publish(store: &Store, scope: &WriteScope, files: Vec<(LibraryFile, Vec<UnitKey>)>) {
    let publications = files
        .iter()
        .map(|(file, units)| StorePublication {
            file,
            units,
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        })
        .collect::<Vec<_>>();
    store.publish_files(scope, &publications, 1000).unwrap();
}

fn work_fields(
    title: &str,
    rating: Option<f64>,
    runtime: Option<u32>,
    language: Option<&str>,
    genres: &[(&str, &str)],
) -> MetadataFields {
    MetadataFields {
        title: FieldValue::Value(title.to_string()),
        rating: rating.map_or(FieldValue::Missing, FieldValue::Value),
        runtime_minutes: runtime.map_or(FieldValue::Missing, FieldValue::Value),
        language: language.map_or(FieldValue::Missing, |value| FieldValue::Value(value.to_string())),
        genres: FieldValue::Value(
            genres
                .iter()
                .map(|(filter_key, name)| MetadataGenre {
                    source: "tmdb".into(),
                    source_id: Some((*filter_key).to_string()),
                    name: (*name).to_string(),
                    filter_key: (*filter_key).to_string(),
                })
                .collect(),
        ),
        ..MetadataFields::default()
    }
}

/// Adopt metadata before publishing: the reducer reads the adopted row when it builds the
/// `library_items` summary, exactly like the real pipeline does.
fn adopt(store: &Store, media_id: MediaId, locale: &str, fields: MetadataFields) {
    store
        .adopt_metadata(
            &MetadataAdoption {
                media_id,
                scope: EntityScope::Work,
                locale: locale.to_string(),
                source: "tmdb".into(),
                provider_entity_id: "1".into(),
                provider_work_id: "1".into(),
                fetched_at: 1000,
                normalized: NormalizedMetadata {
                    fields,
                    field_origins: std::collections::BTreeMap::new(),
                },
                expected_revision: None,
                preferences: MetadataPreferences {
                    language_priority: vec![locale.to_string()],
                    cert_country_priority: vec!["US".into()],
                },
            },
            1000,
        )
        .unwrap();
}

fn query(
    library_id: LibraryId,
    filter: BrowseFilter,
    order: BrowseOrder,
    descending: bool,
    page: BrowsePage,
) -> LibraryQuery {
    LibraryQuery {
        library_id,
        user_id: None,
        filter,
        order,
        descending,
        page,
        include_total: true,
    }
}

fn offset(start: usize, limit: usize) -> BrowsePage {
    BrowsePage::Offset { start, limit }
}

const ALPHA: &str = "00000000-0000-0000-0000-0000000000a1";
const BRAVO: &str = "00000000-0000-0000-0000-0000000000b2";
const CHARLIE: &str = "00000000-0000-0000-0000-0000000000c3";

/// Three movies: Alpha (2024, rating 9, 130 min, en, genre 28, 2160p HDR),
/// Bravo (2015, rating 7, 100 min, ja, genre 18, 1080p), Charlie (1985, no metadata, 720p).
fn seeded() -> (tempfile::TempDir, Store, store::Library, WriteScope) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (library, root) = create_library(&store, "Movies", MediaKind::Movie, "/movies");
    let scope = WriteScope {
        library_id: library.id.parse().unwrap(),
        config_revision: 0,
    };
    let alpha = create_media(&store, ALPHA, "Alpha", MediaKind::Movie, Some(2024), Some("1"));
    let bravo = create_media(&store, BRAVO, "Bravo", MediaKind::Movie, Some(2015), Some("2"));
    let charlie = create_media(&store, CHARLIE, "Charlie", MediaKind::Movie, Some(1985), None);
    adopt(&store, alpha.id, "en-US", work_fields("Alpha", Some(9.0), Some(130), Some("en"), &[("28", "Action")]));
    adopt(&store, bravo.id, "en-US", work_fields("Bravo", Some(7.0), Some(100), Some("ja"), &[("18", "Drama")]));
    publish(
        &store,
        &scope,
        vec![
            (
                make_file(&library, &root, alpha.id, FileShape {
                    file_id: "00000000-0000-0000-0001-0000000000a1",
                    path: "Alpha.mkv",
                    resolution: Some("2160p"),
                    hdr: Some("HDR10"),
                    presence: FilePresence::Present,
                }),
                vec![UnitKey::WHOLE],
            ),
            (
                make_file(&library, &root, bravo.id, FileShape {
                    file_id: "00000000-0000-0000-0001-0000000000b2",
                    path: "Bravo.mkv",
                    resolution: Some("1080p"),
                    hdr: None,
                    presence: FilePresence::Present,
                }),
                vec![UnitKey::WHOLE],
            ),
            (
                make_file(&library, &root, charlie.id, FileShape {
                    file_id: "00000000-0000-0000-0001-0000000000c3",
                    path: "Charlie.mkv",
                    resolution: Some("720p"),
                    hdr: None,
                    presence: FilePresence::Missing,
                }),
                vec![UnitKey::WHOLE],
            ),
        ],
    );
    (tmp, store, library, scope)
}


//! `adopted_filter_facts`: browse filters read the same adopted row the web library uses.

use domain::{
    EntityScope, FieldValue, Media, MediaId, MediaKind, MetadataAdoption, MetadataFields,
    MetadataGenre, MetadataPreferences, NormalizedMetadata,
};
use store::Store;

fn media(store: &Store, id: &str, title: &str) -> Media {
    let media = Media {
        id: id.parse().unwrap(),
        kind: MediaKind::Movie,
        title: title.to_string(),
        year: Some(2024),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: Vec::new(),
    };
    store.insert_media(&media).unwrap();
    media
}

fn fields(genres: &[&str], studios: &[&str], countries: &[&str]) -> MetadataFields {
    MetadataFields {
        genres: FieldValue::Value(
            genres
                .iter()
                .map(|name| MetadataGenre {
                    source: "tmdb".into(),
                    source_id: None,
                    name: (*name).to_string(),
                    filter_key: name.to_lowercase(),
                })
                .collect(),
        ),
        studios: FieldValue::Value(studios.iter().map(|name| (*name).to_string()).collect()),
        countries: FieldValue::Value(countries.iter().map(|name| (*name).to_string()).collect()),
        ..MetadataFields::default()
    }
}

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
                expected_revision: Some(0),
                preferences: MetadataPreferences {
                    language_priority: vec![locale.to_string()],
                    cert_country_priority: vec!["US".into()],
                },
            },
            1000,
        )
        .unwrap();
}

const ALPHA: &str = "00000000-0000-0000-0000-0000000000a1";
const BRAVO: &str = "00000000-0000-0000-0000-0000000000b2";

#[test]
fn adopted_filter_facts_reads_the_adopted_row_per_media() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let alpha = media(&store, ALPHA, "Alpha");
    let bravo = media(&store, BRAVO, "Bravo");
    adopt(
        &store,
        alpha.id,
        "en-US",
        fields(&["Action", "Drama"], &["Studio X"], &["US", "KR"]),
    );
    adopt(
        &store,
        bravo.id,
        "zh-CN",
        fields(&["Comedy"], &["Studio Y"], &[]),
    );

    let facts = store
        .adopted_filter_facts(&[alpha.id, bravo.id], "en-US")
        .unwrap();
    assert_eq!(facts[&alpha.id].genres, ["Action", "Drama"]);
    assert_eq!(facts[&alpha.id].studios, ["Studio X"]);
    assert_eq!(facts[&alpha.id].countries, ["US", "KR"]);
    // Bravo has no `en-US` row: the deterministic `ORDER BY locale` row still applies.
    assert_eq!(facts[&bravo.id].genres, ["Comedy"]);
    assert_eq!(facts[&bravo.id].studios, ["Studio Y"]);
    assert!(facts[&bravo.id].countries.is_empty());
}

#[test]
fn adopted_filter_facts_skips_media_without_an_adopted_row() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let alpha = media(&store, ALPHA, "Alpha");
    adopt(&store, alpha.id, "en-US", fields(&["Action"], &[], &[]));

    // Media with no adopted row is absent, so the caller falls back to NFO for it only.
    let unknown = MediaId::new();
    let facts = store.adopted_filter_facts(&[unknown], "en-US").unwrap();
    assert!(facts.is_empty());

    // An empty candidate set issues no query.
    assert!(store.adopted_filter_facts(&[], "en-US").unwrap().is_empty());
}

#[test]
fn adopted_filter_facts_tolerates_unexpected_payload_shapes() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let alpha = media(&store, ALPHA, "Alpha");
    adopt(
        &store,
        alpha.id,
        "en-US",
        fields(&["Action"], &["Studio X"], &[]),
    );
    // `json_valid` accepts any JSON, including shapes this reader never writes. One odd
    // payload must not fail the whole batch.
    rusqlite::Connection::open(tmp.path().join("library.db"))
        .unwrap()
        .execute(
            "UPDATE media_metadata SET details_json = '[]' WHERE media_id = ?1",
            [ALPHA],
        )
        .unwrap();

    let facts = store.adopted_filter_facts(&[alpha.id], "en-US").unwrap();
    assert_eq!(facts[&alpha.id].genres, ["Action"]);
    assert!(facts[&alpha.id].studios.is_empty());
}

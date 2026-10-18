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
    let normalized = media::normalize_metadata("tmdb", EntityScope::Work, raw_payload, &pref).unwrap();

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

    let rev = store.adopt_metadata(&adoption, 1000).expect("adopt_metadata");
    assert!(rev >= 1);

    // Read back effective metadata
    let effective = store.get_effective_metadata(m.id, "zh-CN").unwrap().expect("effective exists");
    assert_eq!(effective.title, "Adopted Title");
    assert_eq!(effective.overview.as_deref(), Some("Adopted Overview"));
}

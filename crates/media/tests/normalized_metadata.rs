use domain::{EntityScope, FieldValue, MetadataFields, MetadataPreferences, SourceSelection};
use media::normalize_metadata;

#[test]
fn tmdb_movie_certification_priorities_pick_first_matching_country() {
    let raw_payload = r#"{
        "title": "Inception",
        "release_dates": {
            "results": [
                {
                    "iso_3166_1": "US",
                    "release_dates": [{"certification": "PG-13"}]
                },
                {
                    "iso_3166_1": "GB",
                    "release_dates": [{"certification": "12A"}]
                },
                {
                    "iso_3166_1": "CN",
                    "release_dates": [{"certification": "15"}]
                }
            ]
        }
    }"#;

    // Preference 1: [CN, US] -> Should select CN ("15")
    let pref_cn = MetadataPreferences {
        language_priority: vec!["zh-CN".into()],
        cert_country_priority: vec!["CN".into(), "US".into()],
    };
    let norm_cn = normalize_metadata("tmdb", EntityScope::Work, raw_payload, &pref_cn).unwrap();
    assert_eq!(norm_cn.fields.content_rating, FieldValue::Value("15".into()));
    let origin_cn = norm_cn.field_origins.get("content_rating").unwrap();
    assert_eq!(origin_cn.selection, SourceSelection::Certification);
    assert_eq!(origin_cn.certification_country.as_deref(), Some("CN"));

    // Preference 2: [GB, US, CN] -> Should select GB ("12A")
    let pref_gb = MetadataPreferences {
        language_priority: vec!["en-US".into()],
        cert_country_priority: vec!["GB".into(), "US".into(), "CN".into()],
    };
    let norm_gb = normalize_metadata("tmdb", EntityScope::Work, raw_payload, &pref_gb).unwrap();
    assert_eq!(norm_gb.fields.content_rating, FieldValue::Value("12A".into()));
    let origin_gb = norm_gb.field_origins.get("content_rating").unwrap();
    assert_eq!(origin_gb.selection, SourceSelection::Certification);
    assert_eq!(origin_gb.certification_country.as_deref(), Some("GB"));
}

#[test]
fn translation_fallback_records_actual_locale_and_pointer() {
    let raw_payload = r#"{
        "title": "Original Title",
        "overview": "Original Overview",
        "translations": {
            "translations": [
                {
                    "iso_639_1": "fr",
                    "iso_3166_1": "CA",
                    "data": {
                        "overview": "Description en français canadien",
                        "tagline": ""
                    }
                },
                {
                    "iso_639_1": "zh",
                    "iso_3166_1": "CN",
                    "data": {
                        "overview": "中文概述",
                        "tagline": "中文标语"
                    }
                }
            ]
        }
    }"#;

    // Prioritize fr-FR then fr-CA
    let pref = MetadataPreferences {
        language_priority: vec!["fr-FR".into(), "fr-CA".into(), "zh-CN".into()],
        cert_country_priority: vec!["US".into()],
    };
    let norm = normalize_metadata("tmdb", EntityScope::Work, raw_payload, &pref).unwrap();
    assert_eq!(
        norm.fields.overview,
        FieldValue::Value("Description en français canadien".into())
    );
    let origin = norm.field_origins.get("overview").unwrap();
    assert_eq!(origin.selection, SourceSelection::Translation);
    assert_eq!(origin.actual_locale.as_deref(), Some("fr-CA"));

    // Tagline in fr-CA is empty (""), so it must be empty or fallback
    assert!(norm.field_origins.contains_key("tagline"));
}

#[test]
fn missing_vs_empty_field_origin_contracts() {
    let raw_payload = r#"{
        "title": "Minimal",
        "tagline": ""
    }"#;
    let pref = MetadataPreferences {
        language_priority: vec!["en-US".into()],
        cert_country_priority: vec!["US".into()],
    };
    let norm = normalize_metadata("tmdb", EntityScope::Work, raw_payload, &pref).unwrap();

    // title is Value -> has origin
    assert!(matches!(norm.fields.title, FieldValue::Value(_)));
    assert!(norm.field_origins.contains_key("title"));

    // tagline is Empty ("") -> has origin
    assert_eq!(norm.fields.tagline, FieldValue::Empty);
    assert!(norm.field_origins.contains_key("tagline"));

    // overview is not in JSON -> Missing -> MUST NOT have origin
    assert_eq!(norm.fields.overview, FieldValue::Missing);
    assert!(!norm.field_origins.contains_key("overview"));
}

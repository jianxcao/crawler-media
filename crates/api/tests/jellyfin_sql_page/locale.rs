//! Display-locale search uses adopted metadata and production projections.
use super::*;

/// R23: adopted work fields are read in the Library display locale, not the
/// lexically first locale.
#[test]
fn search_reads_the_library_display_locale() {
    let fx = Fx::new();
    let movie = fx.add_movie("Localized");
    let operation = uuid::Uuid::new_v4().to_string();
    let mut status = fx
        .store
        .begin_library_reconfiguration(
            &store::LibraryReconfiguration {
                operation_id: operation.clone(),
                library_id: fx.alpha,
                expected_revision: fx
                    .store
                    .library_config_revision(&fx.alpha.to_string())
                    .unwrap(),
                root_paths: None,
                display_locale: Some("zh-CN".into()),
            },
            1,
        )
        .unwrap();
    for step in 2..100 {
        if status.state == "succeeded" {
            break;
        }
        assert_ne!(status.state, "failed", "{status:?}");
        status = fx
            .store
            .advance_library_operation(&operation, step, 10)
            .unwrap();
    }
    assert_eq!(status.state, "succeeded");
    for (locale, overview) in [("en-US", "english plot"), ("zh-CN", "chinese plot")] {
        fx.store
            .adopt_metadata(
                &MetadataAdoption {
                    media_id: movie,
                    scope: EntityScope::Work,
                    locale: locale.into(),
                    source: "tmdb".into(),
                    provider_entity_id: "100".into(),
                    provider_work_id: "100".into(),
                    fetched_at: 1,
                    normalized: NormalizedMetadata {
                        fields: MetadataFields {
                            title: FieldValue::Value("Localized".into()),
                            overview: FieldValue::Value(overview.into()),
                            ..Default::default()
                        },
                        field_origins: BTreeMap::new(),
                    },
                    expected_revision: Some(0),
                    preferences: MetadataPreferences {
                        language_priority: vec![locale.into()],
                        cert_country_priority: vec![],
                    },
                },
                100,
            )
            .unwrap();
    }
    let mut query = base_query(ProtocolParent::Library(fx.alpha));
    query.search_term = Some("chinese".into());
    assert_eq!(fx.store.protocol_item_page(&query).unwrap().total, 1);
    query.search_term = Some("english".into());
    assert_eq!(
        fx.store.protocol_item_page(&query).unwrap().total,
        0,
        "the lexically first locale must not stand in for the display locale"
    );
}

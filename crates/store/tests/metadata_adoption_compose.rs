use domain::{EntityScope, MetadataAdoption, MetadataPreferences};
use store::Store;

fn provider_id(source: &str) -> &'static str {
    if source == "douban" { "2" } else { "1" }
}

fn adoption(media: domain::MediaId, source: &str, overview: &str) -> MetadataAdoption {
    let mut fields = domain::MetadataFields::default();
    fields.title = domain::FieldValue::Value("Priority Title".into());
    fields.overview = domain::FieldValue::Value(overview.into());
    MetadataAdoption {
        media_id: media, scope: EntityScope::Work, locale: "zh-CN".into(), source: source.into(),
        provider_entity_id: provider_id(source).into(), provider_work_id: provider_id(source).into(),
        fetched_at: 1000,
        normalized: domain::NormalizedMetadata { fields, field_origins: Default::default() },
        expected_revision: Some(0),
        preferences: MetadataPreferences { language_priority: vec!["zh-CN".into()], cert_country_priority: vec!["CN".into()] },
    }
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

fn identity_fixture() -> (tempfile::TempDir, Store, domain::Media, domain::Media) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let make = |id: &str, title: &str| domain::Media {
        id: id.parse().unwrap(),
        kind: domain::MediaKind::Movie,
        title: title.into(),
        year: Some(2024),
        original_title: None,
        aliases: vec![],
        tmdb_id: Some(id.into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let a = make("00000000-0000-0000-0000-0000000000e1", "A");
    let b = make("00000000-0000-0000-0000-0000000000e2", "B");
    store.insert_media(&a).unwrap();
    store.insert_media(&b).unwrap();
    (tmp, store, a, b)
}

fn targeted(media: &domain::Media, title: &str, expected: Option<i64>) -> MetadataAdoption {
    let mut input = adoption(media.id, "tmdb", "o");
    input.normalized.fields.title = domain::FieldValue::Value(title.into());
    input.provider_entity_id = media.id.to_string();
    input.provider_work_id = media.id.to_string();
    input.expected_revision = expected;
    input
}

fn merge_request(a: &domain::Media, b: &domain::Media) -> store::playback::MediaMergeRequest {
    store::playback::MediaMergeRequest {
        request_id: "00000000-0000-0000-0001-0000000000e9".into(),
        survivor: a.id,
        loser: b.id,
        survivor_revision: 0,
        loser_revision: 0,
        refresh_targets: std::collections::BTreeMap::from([("tmdb".into(), a.id.to_string())]),
    }
}

/// R04: a retired tombstone or a frozen (merging) Media accepts no metadata write.
#[test]
fn retired_and_merging_media_refuse_metadata_writes() {
    let (_tmp, store, a, b) = identity_fixture();
    let op = store.begin_media_merge(&merge_request(&a, &b), 10).unwrap();
    let frozen = store.adopt_metadata(&targeted(&b, "late", Some(0)), 11);
    assert!(
        matches!(frozen, Err(store::StoreError::Protected(_))),
        "got {frozen:?}"
    );
    assert_eq!(
        store.advance_media_merge(&op, 12).unwrap().state,
        "succeeded"
    );
    let before = store.get_effective_metadata(b.id, "zh-CN").unwrap();
    let retired_write = store.adopt_metadata(&targeted(&b, "late", Some(0)), 13);
    assert!(
        matches!(retired_write, Err(store::StoreError::Protected(_))),
        "got {retired_write:?}"
    );
    let mut fields = domain::MetadataFields::default();
    fields.title = domain::FieldValue::Value("locked".into());
    let retired_override =
        store.set_metadata_overrides(b.id, EntityScope::Work, "zh-CN", &fields, 0);
    assert!(
        matches!(retired_override, Err(store::StoreError::Protected(_))),
        "got {retired_override:?}"
    );
    assert_eq!(store.get_effective_metadata(b.id, "zh-CN").unwrap(), before);
    assert!(
        store
            .get_metadata_overrides(b.id, EntityScope::Work, "zh-CN")
            .unwrap()
            .is_none()
    );
}

/// R04: adoption is a CAS on the captured Media revision and the current refresh target.
#[test]
fn stale_revision_or_target_adoption_writes_nothing() {
    let (_tmp, store, a, _b) = identity_fixture();
    let missing = store.adopt_metadata(&targeted(&a, "x", None), 1);
    assert!(
        matches!(missing, Err(store::StoreError::Invalid(_))),
        "got {missing:?}"
    );

    let captured = store.media_revision(a.id).unwrap().unwrap();
    let mut renamed = a.clone();
    renamed.title = "A2".into();
    store.update_media_identity(captured, &renamed).unwrap();
    let stale = store.adopt_metadata(&targeted(&a, "stale", Some(captured)), 2);
    assert!(
        matches!(stale, Err(store::StoreError::Conflict(_))),
        "got {stale:?}"
    );

    let current = store.media_revision(a.id).unwrap().unwrap();
    let mut foreign = targeted(&a, "foreign", Some(current));
    foreign.provider_work_id = "999".into();
    foreign.provider_entity_id = "999".into();
    let wrong_target = store.adopt_metadata(&foreign, 3);
    assert!(
        matches!(wrong_target, Err(store::StoreError::Conflict(_))),
        "got {wrong_target:?}"
    );
    assert!(
        store
            .get_effective_metadata(a.id, "zh-CN")
            .unwrap()
            .is_none(),
        "zero writes"
    );

    store
        .adopt_metadata(&targeted(&a, "fresh", Some(current)), 4)
        .unwrap();
    let effective = store
        .get_effective_metadata(a.id, "zh-CN")
        .unwrap()
        .unwrap();
    assert_eq!(effective.title, "fresh");
}

fn set_display_locale(store: &Store, library: domain::LibraryId, locale: &str, now: i64) {
    let operation_id = uuid::Uuid::new_v4().to_string();
    store
        .begin_library_reconfiguration(
            &store::LibraryReconfiguration {
                operation_id: operation_id.clone(),
                library_id: library,
                expected_revision: store.library_config_revision(&library.to_string()).unwrap(),
                root_paths: None,
                display_locale: Some(locale.into()),
            },
            now,
        )
        .unwrap();
    for step in 0..20 {
        let status = store
            .advance_library_operation(&operation_id, now + 1 + step, 100)
            .unwrap();
        if status.state == "succeeded" {
            return;
        }
    }
    panic!("reconfiguration did not finish");
}

fn localized(media: domain::MediaId, locale: &str, title: &str) -> MetadataAdoption {
    let mut adoption = adoption(media, "tmdb", "plot");
    adoption.locale = locale.into();
    adoption.normalized.fields.title = domain::FieldValue::Value(title.into());
    adoption
}

/// C12: card and default detail resolve the Library locale by the same rule
/// (exact, then language prefix); an explicit request still wins for the detail.
#[test]
fn card_and_default_detail_share_the_scoped_locale_rule() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("m");
    let (library, media) = movie_with_library(&store, &root);
    store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: media.id,
            path: root.join("Base.mkv").display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    std::fs::write(root.join("Base.mkv"), b"x").unwrap();
    store
        .adopt_metadata(&localized(media.id, "en-US", "English"), 1)
        .unwrap();
    store
        .adopt_metadata(&localized(media.id, "zh-CN", "中文"), 2)
        .unwrap();
    set_display_locale(&store, library, "zh", 10);
    let detail = |locale: &str| {
        store
            .library_item_detail(library, media.id, locale)
            .unwrap()
            .unwrap()
    };
    let default_card_title = detail("").item.title;
    let default_detail_title = detail("")
        .metadata
        .title
        .value()
        .cloned()
        .unwrap_or_default();
    let explicit_english_detail_title = detail("en-US")
        .metadata
        .title
        .value()
        .cloned()
        .unwrap_or_default();
    assert_eq!(default_card_title, "中文");
    assert_eq!(default_card_title, default_detail_title);
    assert_eq!(explicit_english_detail_title, "English");
}

#[path = "query_model_support/mod.rs"]
mod query_model_support;
use query_model_support::setup_count;

#[test]
fn default_locale_change_freezes_all_libraries_atomically_and_resumes() {
    let (tmp, store, library, ids) = setup_count(201);
    let file = store
        .library_file_page(library, ids[0].parse().unwrap(), None, 1)
        .unwrap()
        .files
        .remove(0);
    let old_scope = store::library_write::WriteScope {
        library_id: library,
        config_revision: 0,
    };
    let changed = store
        .change_metadata_language(0, "en-US", "locale-change", 1)
        .unwrap();
    assert_eq!(changed.revision, 1);
    assert!(
        store
            .publish_files(
                &old_scope,
                &[store::library_write::FilePublication {
                    file: &file,
                    units: &[domain::UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false
                }],
                2
            )
            .is_err()
    );
    assert!(matches!(
        store.ensure_library_query_ready(library),
        Err(store::StoreError::QueryUnavailable { .. })
    ));
    assert_eq!(
        store.library_lifecycle_state(&library.to_string()).unwrap(),
        "reindexing"
    );
    let target = changed
        .operations
        .iter()
        .find(|op| op.library_id == library)
        .unwrap();
    assert_eq!(
        store
            .advance_library_operation(&target.operation_id, 2, 100)
            .unwrap()
            .state,
        "applying"
    );
    drop(store);
    let store = store::Store::open(tmp.path()).unwrap();
    assert!(
        store
            .change_metadata_language(1, "ja", "conflict", 3)
            .is_err()
    );
    for op in &changed.operations {
        for n in 0..5 {
            if store
                .advance_library_operation(&op.operation_id, 4 + n, 100)
                .unwrap()
                .state
                == "succeeded"
            {
                break;
            }
        }
        assert_eq!(
            store
                .library_lifecycle_state(&op.library_id.to_string())
                .unwrap(),
            "ready"
        );
    }
    assert_eq!(store.metadata_language().unwrap().revision, 1);
    assert!(
        store
            .publish_files(
                &old_scope,
                &[store::library_write::FilePublication {
                    file: &file,
                    units: &[domain::UnitKey::WHOLE],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false
                }],
                10
            )
            .is_err()
    );
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let total:i64=db.query_row("SELECT COUNT(*) FROM library_items WHERE library_id=?1 AND query_config_revision=1 AND query_projection_revision=item_revision",[library.to_string()],|r|r.get(0)).unwrap();
    assert_eq!(total, 201);
    assert_eq!(
        store
            .change_metadata_language(1, "en-US", "same", 10)
            .unwrap()
            .revision,
        1
    );
    assert!(store.put_setting("metadata.language", "ja").is_err());
    assert!(store.delete_setting("metadata.language").is_err());
}

#[test]
fn conflict_is_zero_write_and_integrity_ownership_is_bidirectional() {
    let (_tmp, store, library, _) = setup_count(1);
    store
        .begin_library_integrity(library, true, "integrity", 0)
        .unwrap();
    assert!(matches!(
        store.change_metadata_language(0, "ja", "blocked", 1),
        Err(store::StoreError::Protected(_))
    ));
    assert_eq!(store.metadata_language().unwrap().revision, 0);
    assert!(
        (0..10_000).any(|_| store
            .advance_library_operation("integrity", 2, 100)
            .unwrap()
            .state
            == "succeeded"),
        "integrity operation did not succeed within 10_000 steps"
    );
    let changed = store
        .change_metadata_language(0, "ja", "change", 3)
        .unwrap();
    assert!(
        store
            .begin_library_integrity(library, true, "blocked-integrity", 4)
            .is_err()
    );
    assert!(matches!(
        store.ensure_library_query_ready(library),
        Err(store::StoreError::QueryUnavailable { .. })
    ));
    for op in changed.operations {
        assert!(
            (0..10_000).any(|_| store
                .advance_library_operation(&op.operation_id, 5, 100)
                .unwrap()
                .state
                == "succeeded"),
            "locale operation did not succeed within 10_000 steps"
        );
    }
    let cleared = store.change_metadata_language(1, "  ", "clear", 6).unwrap();
    assert_eq!(cleared.language, "");
    assert_eq!(cleared.revision, 2);
}

#[test]
fn failed_default_locale_retains_owner_and_retries_same_cursor() {
    let (tmp, store, library, _) = setup_count(2);
    let changed = store
        .change_metadata_language(0, "en-US", "change", 0)
        .unwrap();
    let op = changed
        .operations
        .iter()
        .find(|op| op.library_id == library)
        .unwrap();
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_card BEFORE UPDATE ON library_items BEGIN SELECT RAISE(ABORT,'locale fault'); END;").unwrap();
    assert!(
        store
            .advance_library_operation(&op.operation_id, 1, 100)
            .is_err()
    );
    assert_eq!(
        store
            .metadata_language()
            .unwrap()
            .operations
            .iter()
            .find(|x| x.library_id == library)
            .unwrap()
            .state,
        "failed"
    );
    assert!(
        store
            .begin_library_integrity(library, true, "blocked", 2)
            .is_err()
    );
    db.execute_batch("DROP TRIGGER fail_card").unwrap();
    drop(store);
    let store = store::Store::open(tmp.path()).unwrap();
    assert!(
        store
            .unfinished_library_operations()
            .unwrap()
            .contains(&op.operation_id)
    );
    assert_eq!(
        store
            .advance_library_operation(&op.operation_id, 3, 100)
            .unwrap()
            .state,
        "succeeded"
    );
}

#[test]
fn inherited_language_changes_cards_preserves_explicit_locale_and_stamps_new_library() {
    let (tmp, store, library, ids) = setup_count(1);
    for (locale, title) in [("en-US", "English"), ("zh-CN", "中文")] {
        let fields =
            serde_json::from_value(serde_json::json!({"title":{"state":"value","value":title}}))
                .unwrap();
        store
            .set_metadata_overrides(
                ids[0].parse().unwrap(),
                domain::EntityScope::Work,
                locale,
                &fields,
                if locale == "en-US" { 1 } else { 0 },
            )
            .unwrap();
    }
    let explicit = store
        .create_library(
            domain::MediaKind::Movie,
            "Explicit",
            &["/explicit"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = ids[0].parse().unwrap();
    assert_library_title(&store, library, media, "中文");
    store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: media,
            path: "/explicit/movie.mkv".into(),
            ..Default::default()
        },
    )
    .unwrap();
    set_library_locale(&store, explicit.id.parse().unwrap(), "zh-CN", "explicit");
    let changed = store
        .change_metadata_language(0, "en-US", "change", 0)
        .unwrap();
    let new = store
        .create_library(
            domain::MediaKind::Movie,
            "New",
            &["/new"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .ensure_library_query_ready(new.id.parse().unwrap())
        .unwrap();
    for op in &changed.operations {
        assert_eq!(
            store
                .advance_library_operation(&op.operation_id, 1, 100)
                .unwrap()
                .state,
            "succeeded"
        );
    }
    assert_library_title(&store, library, media, "English");
    assert_library_title(&store, explicit.id.parse().unwrap(), media, "中文");
    set_library_locale(&store, explicit.id.parse().unwrap(), "", "inherit");
    assert_library_title(&store, explicit.id.parse().unwrap(), media, "English");
    store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: media,
            path: "/new/movie.mkv".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_library_title(&store, new.id.parse().unwrap(), media, "English");
    let cleared = store
        .change_metadata_language(1, "", "clear-default", 2)
        .unwrap();
    for operation in cleared.operations {
        assert_eq!(
            query_model_support::finish(&store, &operation.operation_id).0,
            "succeeded"
        );
    }
    for library in [
        library,
        explicit.id.parse().unwrap(),
        new.id.parse().unwrap(),
    ] {
        assert_library_title(&store, library, media, "中文");
    }
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let revisions:(i64,i64)=db.query_row("SELECT query_config_revision,query_projection_revision=item_revision FROM library_items WHERE library_id=?1",[library.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(revisions, (2, 1));
    query_model_support::run(&store, library, false, "oracle");
    assert!(
        !store
            .library_integrity_report("oracle")
            .unwrap()
            .has_issues()
    );
}

fn set_library_locale(
    store: &store::Store,
    library: domain::LibraryId,
    locale: &str,
    operation: &str,
) {
    store
        .begin_library_reconfiguration(
            &store::LibraryReconfiguration {
                operation_id: operation.into(),
                library_id: library,
                expected_revision: store.library_config_revision(&library.to_string()).unwrap(),
                root_paths: None,
                display_locale: Some(locale.into()),
            },
            0,
        )
        .unwrap();
    assert_eq!(query_model_support::finish(store, operation).0, "succeeded");
}

fn assert_library_title(
    store: &store::Store,
    library: domain::LibraryId,
    media: domain::MediaId,
    title: &str,
) {
    use store::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
    let page = store
        .library_item_page(&LibraryQuery {
            library_id: library,
            user_id: None,
            filter: BrowseFilter::default(),
            order: BrowseOrder::Title,
            descending: false,
            page: BrowsePage::Offset {
                start: 0,
                limit: 10,
            },
            include_total: false,
        })
        .unwrap();
    assert_eq!(page.items[0].title, title);
    let detail = store
        .library_item_detail(library, media, "")
        .unwrap()
        .unwrap();
    assert_eq!(detail.item.title, title);
    assert_eq!(
        detail.metadata.title,
        domain::FieldValue::Value(title.into())
    );
    let protocol = store
        .protocol_page_metadata(library, media, domain::EntityScope::Work, None)
        .unwrap();
    assert_eq!(protocol.title, domain::FieldValue::Value(title.into()));
    let hydrated = store.page_hydration(&[detail.files[0].file_id]).unwrap();
    assert_eq!(
        hydrated[&detail.files[0].file_id].fields[&domain::EntityScope::Work].title,
        domain::FieldValue::Value(title.into())
    );
}

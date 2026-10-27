mod query_model_support;
use query_model_support::*;
#[test]
fn integrity_blocks_metadata_and_playback_until_completion() {
    let (_tmp, store, lib, ids) = setup();
    let media = ids[0].parse().unwrap();
    store
        .begin_library_integrity(lib, true, "repair", 0)
        .unwrap();
    store.advance_library_operation("repair", 1, 1).unwrap();
    assert!(matches!(
        store.set_metadata_overrides(
            media,
            domain::EntityScope::Work,
            "en-US",
            &domain::MetadataFields::default(),
            1
        ),
        Err(store::StoreError::Protected(_))
    ));
    let mutation =
        store::playback::PlaybackMutation::marks(domain::UserId::new(), -1, -1, None, Some(true));
    assert!(matches!(
        store.mutate_playback(media, &mutation, 2),
        Err(store::StoreError::Protected(_))
    ));
    assert!(
        store
            .begin_library_integrity(lib, false, "second", 2)
            .is_err()
    );
}

#[test]
fn integrity_blocks_changes_to_already_staged_media() {
    let (tmp, store, lib, ids) = setup_count(201);
    let media = ids[0].parse().unwrap();
    let user = domain::UserId::new();
    let collection = store.create_collection(user, "Frozen").unwrap();
    store.add_collection_item(&collection, &ids[0]).unwrap();
    store
        .begin_library_integrity(lib, true, "staged", 0)
        .unwrap();
    assert_eq!(
        store
            .advance_library_operation("staged", 1, 100)
            .unwrap()
            .state,
        "applying"
    );
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let before = snapshot(&db);
    assert_frozen_fact_writes(&store, lib, media, user);
    let fields = domain::MetadataFields::default();
    assert!(matches!(
        store.set_metadata_overrides(media, domain::EntityScope::Work, "en-US", &fields, 1),
        Err(store::StoreError::Protected(_))
    ));
    assert!(matches!(
        store.delete_media(media),
        Err(store::StoreError::Protected(_))
    ));
    assert!(matches!(
        store.delete_playback_for_media(media),
        Err(store::StoreError::Protected(_))
    ));
    assert!(matches!(
        store.clear_units(user, Some(&[media]), None),
        Err(store::StoreError::Protected(_))
    ));
    assert!(matches!(
        store.rename_collection(&collection, "changed"),
        Err(store::StoreError::Protected(_))
    ));
    let mut identity = store.get_media(media).unwrap().unwrap();
    identity.title = "changed".into();
    assert!(matches!(
        store.update_media(&identity),
        Err(store::StoreError::Protected(_))
    ));
    assert_eq!(
        snapshot(&db),
        before,
        "rejected writes preserve facts, counters and staging"
    );
    drop(store);
    let store = store::Store::open(tmp.path()).unwrap();
    while store
        .advance_library_operation("staged", 2, 100)
        .unwrap()
        .state
        == "applying"
    {}
    run(&store, lib, false, "oracle");
    assert!(
        !store
            .library_integrity_report("oracle")
            .unwrap()
            .has_issues()
    );
    store
        .set_metadata_overrides(media, domain::EntityScope::Work, "en-US", &fields, 1)
        .unwrap();
    assert!(store.rename_collection(&collection, "changed").unwrap());
}

fn snapshot(db: &rusqlite::Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    [
        "media",
        "media_metadata_overrides",
        "playback_units",
        "library_items",
        "library_item_terms",
        "library_query_counts",
        "library_query_count_rebuild",
        "collections",
    ]
    .iter()
    .map(|table| {
        // Order by all columns, not `rowid`: `library_item_terms` is WITHOUT
        // ROWID (layout v3). Every column list ends with a unique key, so this
        // stays a deterministic, content-only ordering.
        let width = db
            .prepare(&format!("SELECT * FROM {table}"))
            .unwrap()
            .column_count();
        let order = (1..=width)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
            .unwrap();
        stmt.query_map([], |row| {
            (0..width)
                .map(|n| row.get(n))
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    })
    .collect()
}

#[test]
fn final_count_swap_failure_retains_owner_and_resumes_after_reopen() {
    let (tmp, store, lib, _) = setup();
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute("UPDATE library_query_counts SET member_count=0", [])
        .unwrap();
    store.begin_library_integrity(lib, true, "swap", 0).unwrap();
    store.advance_library_operation("swap", 1, 3).unwrap();
    let before = snapshot(&db);
    db.execute_batch("CREATE TRIGGER reject_swap BEFORE DELETE ON library_query_counts BEGIN SELECT RAISE(ABORT, 'swap fault'); END;").unwrap();
    assert!(store.advance_library_operation("swap", 2, 3).is_err());
    assert_eq!(
        snapshot(&db),
        before,
        "final batch is atomic, including its staging"
    );
    assert!(matches!(
        store.ensure_library_query_ready(lib),
        Err(store::StoreError::QueryUnavailable { .. })
    ));
    assert!(
        store
            .begin_library_integrity(lib, true, "other", 3)
            .is_err()
    );
    assert!(
        store
            .unfinished_library_operations()
            .unwrap()
            .contains(&"swap".to_string())
    );
    assert_eq!(
        store
            .active_library_operation(&lib.to_string())
            .unwrap()
            .as_deref(),
        Some("swap")
    );
    db.execute_batch("DROP TRIGGER reject_swap").unwrap();
    drop(store);
    let store = store::Store::open(tmp.path()).unwrap();
    assert_eq!(
        store.advance_library_operation("swap", 4, 3).unwrap().state,
        "succeeded"
    );
    store.ensure_library_query_ready(lib).unwrap();
    run(&store, lib, false, "verify-swap");
    assert!(
        !store
            .library_integrity_report("verify-swap")
            .unwrap()
            .has_issues()
    );
}

fn assert_frozen_fact_writes(
    store: &store::Store,
    lib: domain::LibraryId,
    media: domain::MediaId,
    user: domain::UserId,
) {
    let adoption = domain::MetadataAdoption {
        media_id: media,
        scope: domain::EntityScope::Work,
        locale: "en-US".into(),
        source: "fixture".into(),
        provider_entity_id: "entity".into(),
        provider_work_id: "work".into(),
        fetched_at: 1,
        normalized: domain::NormalizedMetadata {
            fields: domain::MetadataFields::default(),
            field_origins: Default::default(),
        },
        expected_revision: Some(0),
        preferences: domain::MetadataPreferences {
            language_priority: vec![],
            cert_country_priority: vec![],
        },
    };
    assert!(matches!(
        store.adopt_metadata(&adoption, 1),
        Err(store::StoreError::Protected(_))
    ));
    assert!(matches!(
        store.record_scraped_display(
            media,
            "en-US",
            store::metadata::ScrapedDisplay {
                rating: Some(8.0),
                ..Default::default()
            }
        ),
        Err(store::StoreError::Protected(_))
    ));
    assert!(matches!(
        store.upsert_unit(
            user,
            media,
            -1,
            -1,
            0,
            Some(true),
            Some(true),
            None,
            None,
            None,
            false,
            1
        ),
        Err(store::StoreError::Protected(_))
    ));
    let file = store.file_coverage_for_media(media).unwrap()[0]
        .file
        .clone();
    assert!(matches!(
        store.put_file_meta(&file.file_id.to_string(), &library::Tracks::default()),
        Err(store::StoreError::Protected(_))
    ));
    assert!(matches!(
        store.record_file_artwork(
            file.file_id,
            &file.version,
            &store::library_write::FileArtwork {
                poster: Some(std::path::Path::new("/poster.jpg")),
                ..Default::default()
            }
        ),
        Err(store::StoreError::Protected(_))
    ));
    let observed = store
        .plan_nfo_input_reconcile(&file.path)
        .unwrap()
        .unwrap()
        .observe();
    assert!(matches!(
        store.apply_nfo_input_reconcile(observed),
        Err(store::StoreError::Protected(_))
    ));
    assert!(matches!(
        store.delete_files(lib, &[file.file_id]),
        Err(store::StoreError::Protected(_))
    ));
    assert!(matches!(
        store.begin_library_removal(&lib.to_string()),
        Err(store::StoreError::Protected(_))
    ));
    assert!(matches!(
        store.begin_library_reconfiguration(
            &store::LibraryReconfiguration {
                operation_id: "locale".into(),
                library_id: lib,
                expected_revision: 1,
                root_paths: None,
                display_locale: Some("en-US".into())
            },
            1
        ),
        Err(store::StoreError::Protected(_))
    ));
}

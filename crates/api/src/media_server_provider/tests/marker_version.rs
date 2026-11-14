use super::*;
use domain::{Media, MediaId, MediaKind, TypedItemId, UnitKey};

fn fixture_state(root: &std::path::Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
    )
    .unwrap()
}

#[test]
fn direct_item_snapshot_does_not_attach_replacement_markers_to_selected_old_version() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_state(dir.path());
    let store = &state.store;
    let user = store.admin_user_id().unwrap();
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Replacement".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let path = store
        .library_root(MediaKind::Tv)
        .unwrap()
        .join("S01E01.mkv");
    std::fs::write(&path, b"original").unwrap();
    let key = UnitKey::new(1, 1);
    let original = store::test_support::publish_fixture_file_units(
        store,
        &store::test_support::FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            ..Default::default()
        },
        &[key],
    )
    .unwrap();
    let id = TypedItemId::Unit {
        media: media.id,
        key,
    }
    .to_wire_string();
    // Preserve the actual selection before the replacement is registered.
    let selected = crate::media_server_provider::items::resolve_entry(store, Some(user), &id)
        .unwrap()
        .unwrap();
    assert_eq!(selected.file.version, original.version);

    let mut replacement = original.clone();
    replacement.version.revision += 1;
    replacement.version.source_version = "replacement-source".into();
    store
        .publish_files(
            &store::library_write::WriteScope {
                library_id: replacement.library_id,
                config_revision: 0,
            },
            &[store::library_write::FilePublication {
                file: &replacement,
                units: &[key],
                expected_version: Some(&original.version),
                nfo_observation: None,
                probe_required: false,
            }],
            1_000,
        )
        .unwrap();
    store
        .put_fixture_marker(&store::StoredFileMarker {
            file_id: replacement.file_id,
            version: replacement.version.clone(),
            unit: key,
            intro_start_ms: Some(40_000),
            intro_end_ms: Some(100_000),
            outro_start_ms: Some(900_000),
            outro_end_ms: Some(990_000),
            source: "manual".into(),
            locked: true,
            updated_at: 1_000,
        })
        .unwrap();

    let stale = crate::media_server_provider::items::snapshot_from_entry(
        &state,
        user,
        selected.file,
        selected.unit,
        selected.media,
        selected.is_series,
    )
    .unwrap();
    assert_eq!(stale.file.version, original.version);
    assert_eq!(stale.unit, Some(key));
    assert_eq!(
        (
            stale.intro_start_ms,
            stale.intro_end_ms,
            stale.outro_start_ms,
            stale.outro_end_ms
        ),
        (None, None, None, None),
        "the selected V1 response must not emit V2 intervals"
    );

    let current = crate::media_server_provider::items::entry_snapshot(&state, user, &id)
        .unwrap()
        .unwrap();
    assert_eq!(current.file.version, replacement.version);
    assert_eq!(current.unit, Some(key));
    assert_eq!(
        (
            current.intro_start_ms,
            current.intro_end_ms,
            current.outro_start_ms,
            current.outro_end_ms
        ),
        (Some(40_000), Some(100_000), Some(900_000), Some(990_000))
    );
}

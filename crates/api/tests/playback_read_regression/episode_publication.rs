use domain::{Media, MediaId, MediaKind, UnitKey};
use library::TransferMode;
use store::library_write::WriteScope;
use store::test_support::{FixtureFile, publish_fixture_file};

#[tokio::test]
async fn playback_episode_views_exclude_pending_transfer_until_publication() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let state = super::common::make_state(temp.path());
    let app = api::router(state.clone());
    let media_id = MediaId::new();
    let root = temp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let (library_id, first_file) = {
        let handle = state.store();
        let store = handle.as_ref();
        store
            .create_library(
                MediaKind::Tv,
                "TV",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Tv,
                title: "Episodes".into(),
                year: None,
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let first = publish_episode(store, media_id, &root, 1);
        publish_episode(store, media_id, &root, 3);
        (first.library_id, first)
    };
    let handle = state.store();
    let store = handle.as_ref();
    let scope = WriteScope {
        library_id,
        config_revision: store
            .library_config_revision(&library_id.to_string())
            .unwrap(),
    };
    let source = first_file.path.clone();
    let mut candidate = store
        .file_coverage_by_id(first_file.file_id)
        .unwrap()
        .unwrap();
    candidate.file.file_id = domain::LedgerId::new();
    candidate.file.path = root.join("S01E02.mkv");
    candidate.path_key = candidate.file.path.display().to_string();
    candidate.units = vec![UnitKey::new(1, 2)];
    let pending = store
        .reserve_transfer(&scope, &candidate, &source, TransferMode::Copy, 1_000)
        .unwrap();
    drop(handle);

    let season = super::common::get(
        &app,
        &format!("/api/v1/playback/items/{media_id}/episodes?season_number=1"),
        "management-secret",
    )
    .await;
    let all_seasons = super::common::get(
        &app,
        &format!("/api/v1/playback/items/{media_id}/episodes"),
        "management-secret",
    )
    .await;
    for response in [season, all_seasons] {
        let episodes = response["episodes"].as_array().unwrap();
        assert_eq!(episodes.len(), 2);
        assert_eq!(episodes[0]["episode_number"], 1);
        assert_eq!(episodes[1]["episode_number"], 3);
        assert!(episodes.iter().all(|episode| {
            episode["file_ids"]
                .as_array()
                .is_some_and(|ids| !ids.iter().any(|id| id == &pending.file.file_id.to_string()))
        }));
    }

    let handle = state.store();
    let store = handle.as_ref();
    std::fs::copy(&source, &pending.file.path).unwrap();
    store
        .finish_transfer(&scope, &pending, &pending.file.version, 2_000)
        .unwrap();
    let season = super::common::get(
        &app,
        &format!("/api/v1/playback/items/{media_id}/episodes?season_number=1"),
        "management-secret",
    )
    .await;
    assert_eq!(season["episodes"][1]["episode_number"], 2);
}

fn publish_episode(
    store: &store::Store,
    media_id: MediaId,
    root: &std::path::Path,
    episode: u32,
) -> domain::LibraryFile {
    let path = root.join(format!("S01E{episode:02}.mkv"));
    std::fs::write(&path, b"episode").unwrap();
    publish_fixture_file(
        store,
        &FixtureFile {
            media_id,
            path: path.display().to_string(),
            season: Some(1),
            episode: Some(episode),
            ..Default::default()
        },
    )
    .unwrap()
}

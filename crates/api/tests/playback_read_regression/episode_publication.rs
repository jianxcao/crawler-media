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

fn seed_pending_tv(
    store: &store::Store,
    root: &std::path::Path,
    media_id: MediaId,
) -> (WriteScope, domain::FileCoverage) {
    let lib = store
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
            title: "PendingShow".into(),
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
    let library_id: domain::LibraryId = lib.id.parse().unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id == lib.id)
        .unwrap()
        .id;
    let scope = WriteScope {
        library_id,
        config_revision: store.library_config_revision(&lib.id).unwrap(),
    };
    let path = root.join("S01E01.mkv");
    std::fs::write(&path, b"ep").unwrap();
    let cov = domain::FileCoverage {
        path_key: path.display().to_string(),
        file: domain::LibraryFile {
            file_id: domain::LedgerId::new(),
            library_id,
            root_id,
            media_id,
            kind: MediaKind::Tv,
            path: path.clone(),
            presence: domain::FilePresence::Present,
            publication: domain::FilePublication::Pending,
            version: domain::FileVersion {
                revision: 1,
                source_version: "v1".into(),
            },
            size_bytes: Some(2),
            stat_known: true,
            created_ns: None,
            modified_ns: None,
            source_digest: None,
            source_path: None,
            transfer_mode: Some("copy".into()),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: None,
            tracks_version: 0,
            poster_path: None,
            backdrop_path: None,
            still_path: None,
        },
        units: vec![UnitKey::new(1, 1)],
    };
    let pending = store
        .reserve_transfer(&scope, &cov, &path, TransferMode::Copy, 1_000)
        .unwrap();
    (scope, pending)
}

#[tokio::test]
async fn playback_item_and_episodes_return_404_when_only_pending_file_exists() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let state = super::common::make_state(temp.path());
    let app = api::router(state.clone());
    let media_id = MediaId::new();
    let root = temp.path().join("tv-pending");
    std::fs::create_dir_all(&root).unwrap();
    let (scope, pending) = seed_pending_tv(&state.store(), &root, media_id);

    for (expected, finish) in [
        (axum::http::StatusCode::NOT_FOUND, false),
        (axum::http::StatusCode::OK, true),
    ] {
        if finish {
            state
                .store()
                .finish_transfer(&scope, &pending, &pending.file.version, 2_000)
                .unwrap();
        }
        for uri in [
            format!("/api/v1/playback/items/{media_id}"),
            format!("/api/v1/playback/items/{media_id}/episodes"),
            format!("/api/v1/playback/items/{media_id}/episodes?season_number=1"),
        ] {
            let req = axum::http::Request::builder()
                .uri(&uri)
                .header("authorization", "Bearer management-secret")
                .body(axum::body::Body::empty())
                .unwrap();
            let resp = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
            assert_eq!(resp.status(), expected, "{uri}");
        }
    }
}

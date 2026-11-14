//! catalog 大类：show/season/movie 的已看标记如何在明细与汇总上生效。
use super::*;

#[tokio::test]
async fn season_played_mark_updates_episode_rollup_and_next_up() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv-season-mark");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Season mark shows",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = new_media(MediaKind::Tv, "Season mark show");
    store.insert_media(&show).unwrap();
    let first = insert_episode(&store, &root, show.id, 1, 1);
    insert_episode(&store, &root, show.id, 1, 2);
    drop(store);
    let season_id = get_json(&app, &format!("/Shows/{}/Seasons", work_id(show.id))).await["Items"]
        [0]["Id"]
        .as_str()
        .unwrap()
        .to_owned();
    let marked = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/UserPlayedItems/{season_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(marked.status(), StatusCode::OK);
    let episode = get_json(
        &app,
        &format!("/Items/{}", first.to_string().replace('-', "")),
    )
    .await;
    assert_eq!(episode["UserData"]["Played"], true);
    let listing = get_json(&app, "/Items").await;
    let series = listing["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["Name"] == "Season mark show")
        .unwrap();
    assert_eq!(series["UserData"]["Played"], true);
    assert_eq!(series["UserData"]["UnplayedItemCount"], 0);
    assert!(
        get_json(&app, "/Shows/NextUp").await["Items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn newer_show_played_mark_supersedes_older_unplayed_season_everywhere() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv-precedence");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Precedence",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = new_media(MediaKind::Tv, "Precedence show");
    store.insert_media(&show).unwrap();
    let episode = insert_episode(&store, &root, show.id, 1, 1);
    let user = store.admin_user_id().unwrap();
    store
        .upsert_unit(
            user,
            show.id,
            1,
            -1,
            0,
            Some(false),
            None,
            None,
            None,
            None,
            false,
            10,
        )
        .unwrap();
    store
        .upsert_unit(
            user,
            show.id,
            -1,
            -1,
            0,
            Some(true),
            None,
            None,
            None,
            None,
            false,
            20,
        )
        .unwrap();
    drop(store);
    assert_eq!(
        get_json(
            &app,
            &format!("/Items/{}", episode.to_string().replace('-', ""))
        )
        .await["UserData"]["Played"],
        true
    );
    assert!(
        get_json(&app, "/Shows/NextUp").await["Items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn newer_season_unplayed_mark_wins_detail_and_listing() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv-mark-precedence");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Mark precedence",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = new_media(MediaKind::Tv, "Mark precedence show");
    store.insert_media(&show).unwrap();
    let episode = insert_episode(&store, &root, show.id, 1, 1);
    let user = store.admin_user_id().unwrap();
    store
        .upsert_unit(
            user,
            show.id,
            1,
            1,
            0,
            Some(true),
            None,
            None,
            None,
            None,
            false,
            10,
        )
        .unwrap();
    store
        .upsert_unit(
            user,
            show.id,
            1,
            -1,
            0,
            Some(false),
            None,
            None,
            None,
            None,
            false,
            20,
        )
        .unwrap();
    drop(store);

    let episode_id = episode.to_string().replace('-', "");
    assert_eq!(
        get_json(&app, &format!("/Items/{episode_id}")).await["UserData"]["Played"],
        false
    );
    let listing = get_json(&app, "/Items").await;
    let listed_show = listing["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["Name"] == "Mark precedence show")
        .unwrap();
    assert_eq!(listed_show["UserData"]["Played"], false);
    let next_up = get_json(&app, "/Shows/NextUp").await;
    assert!(
        next_up["Items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["Id"] == episode_id)
    );

    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .upsert_unit(
            user,
            show.id,
            1,
            1,
            0,
            Some(true),
            None,
            None,
            None,
            None,
            false,
            30,
        )
        .unwrap();
    drop(store);
    assert_eq!(
        get_json(&app, &format!("/Items/{episode_id}")).await["UserData"]["Played"],
        true
    );
}

#[tokio::test]
async fn movie_marks_by_item_id_use_the_whole_media_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("films");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Films",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let movie = new_media(MediaKind::Movie, "Film");
    store.insert_media(&movie).unwrap();
    let path = root.join("Film.mkv");
    std::fs::write(&path, b"film").unwrap();
    let spec = store::test_support::FixtureFile {
        media_id: movie.id,
        path: path.display().to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        transfer_mode: None,
        source_path: None,
        file_id: None,
    };
    store::test_support::publish_fixture_file(&store, &spec).unwrap();
    let item_id = work_id(movie.id);
    for (route, expected) in [
        (format!("/UserPlayedItems/{item_id}"), "Played"),
        (format!("/UserFavoriteItems/{item_id}"), "Favorite"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(route)
                    .header("authorization", "Bearer admin-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{expected}");
    }
    let state = Store::open(tmp.path().join("data"))
        .unwrap()
        .unit_state(
            domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap(),
            movie.id,
            -1,
            -1,
        )
        .unwrap()
        .unwrap();
    assert!(state.played);
    assert!(state.favorite);
}

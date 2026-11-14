//! catalog 大类：收藏标记与继承已看状态、清空历史之间的交互。
use super::*;

#[tokio::test]
async fn native_season_favorite_does_not_resurrect_explicitly_unplayed_episode() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("native-favorite");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Native favorite",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = new_media(MediaKind::Tv, "Native favorite show");
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
            Some(true),
            None,
            None,
            None,
            None,
            false,
            5,
        )
        .unwrap();
    store
        .upsert_unit(
            user,
            show.id,
            1,
            1,
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
    drop(store);

    let response = app.clone().oneshot(Request::builder()
        .method("POST")
        .uri("/api/v1/playback/marks")
        .header("authorization", "Bearer admin-token")
        .header("content-type", "application/json")
        .body(Body::from(json!({"media_item_id": show.id.to_string(), "season_number": 1, "favorite": true}).to_string()))
        .unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        get_json(
            &app,
            &format!("/Items/{}", episode.to_string().replace('-', ""))
        )
        .await["UserData"]["Played"],
        false
    );
}

#[tokio::test]
async fn favorite_only_episode_does_not_survive_history_clear_as_played() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("favorite-only-episode");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Favorite only",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = new_media(MediaKind::Tv, "Favorite only show");
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
            Some(true),
            None,
            None,
            None,
            None,
            false,
            10,
        )
        .unwrap();
    drop(store);

    let marked = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/UserFavoriteItems/{}",
                    episode.to_string().replace('-', "")
                ))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(marked.status(), StatusCode::OK);
    let cleared = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/playback/history?scope=all")
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cleared.status(), StatusCode::OK);
    assert_eq!(
        get_json(
            &app,
            &format!("/Items/{}", episode.to_string().replace('-', ""))
        )
        .await["UserData"]["Played"],
        false
    );
}

#[tokio::test]
async fn favoriting_episode_does_not_flip_inherited_played_to_unplayed() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("favorite-played-episode");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Favorite played",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = new_media(MediaKind::Tv, "Favorite played show");
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
            Some(true),
            None,
            None,
            None,
            None,
            false,
            0,
        )
        .unwrap();
    drop(store);

    let episode_id = episode.to_string().replace('-', "");
    let marked = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/UserFavoriteItems/{episode_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(marked.status(), StatusCode::OK);
    assert_eq!(
        get_json(&app, &format!("/Items/{episode_id}")).await["UserData"]["Played"],
        true,
        "favouriting must not defeat an inherited played mark"
    );
}

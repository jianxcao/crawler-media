//! catalog 大类：TV 季过滤、Resume 与 show/season 播放标记的组合行为。
//!
//! 原 `tv_season_filter_resume_and_show_played_marks_work` 的 376 行用例按行为阶段
//! 拆为 fixture + 命名步骤；断言文本与步骤顺序逐字保留。

use super::*;

/// TV 库 + 一个 show + S01E01/S01E02/S02E01/S02E02 的会话夹具。
struct TvSeasonFilterFixture {
    tmp: tempfile::TempDir,
    app: axum::Router,
    _store: Store,
    show: Media,
    series_id: String,
    first_id: String,
    second_id: String,
    third_id: String,
    fourth_id: String,
    season_one_id: String,
    season_two_id: String,
}

/// 建库、建 show、铺 4 集，并读出两个 season id。
async fn tv_season_filter_fixture() -> TvSeasonFilterFixture {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = new_media(MediaKind::Tv, "Show");
    store.insert_media(&show).unwrap();
    let first_id = insert_episode(&store, &root, show.id, 1, 1);
    let second_id = insert_episode(&store, &root, show.id, 1, 2);
    let third_id = insert_episode(&store, &root, show.id, 2, 1);
    let fourth_id = insert_episode(&store, &root, show.id, 2, 2);
    let series_id = work_id(show.id);

    let seasons = get_json(&app, &format!("/Shows/{series_id}/Seasons")).await;
    let season_one_id = seasons["Items"][0]["Id"].as_str().unwrap();
    let season_two_id = seasons["Items"][1]["Id"].as_str().unwrap();
    TvSeasonFilterFixture {
        tmp,
        app,
        _store: store,
        show,
        series_id,
        first_id,
        second_id,
        third_id,
        fourth_id,
        season_one_id: season_one_id.to_string(),
        season_two_id: season_two_id.to_string(),
    }
}

/// 剧集列表与 SeasonId 过滤。
async fn episode_listing_reflects_season_filter(fx: &TvSeasonFilterFixture) {
    let app = fx.app.clone();
    let series_id = fx.series_id.clone();
    let season_one_id = fx.season_one_id.clone();
    let first_id = fx.first_id.clone();
    let second_id = fx.second_id.clone();
    let third_id = fx.third_id.clone();
    let fourth_id = fx.fourth_id.clone();
    let all_episodes = get_json(&app, &format!("/Shows/{series_id}/Episodes")).await;
    assert_eq!(all_episodes["TotalRecordCount"], 4);
    assert_eq!(
        all_episodes["Items"][0]["Id"],
        first_id.to_string().replace('-', "")
    );
    assert_eq!(
        all_episodes["Items"][1]["Id"],
        second_id.to_string().replace('-', "")
    );
    assert_eq!(
        all_episodes["Items"][2]["Id"],
        third_id.to_string().replace('-', "")
    );
    assert_eq!(
        all_episodes["Items"][3]["Id"],
        fourth_id.to_string().replace('-', "")
    );

    let season_episodes = get_json(
        &app,
        &format!("/Shows/{series_id}/Episodes?SeasonId={season_one_id}"),
    )
    .await;
    assert_eq!(season_episodes["TotalRecordCount"], 2);
    assert_eq!(season_episodes["Items"].as_array().unwrap().len(), 2);
    assert_eq!(season_episodes["Items"][0]["IndexNumber"], 1);
    assert_eq!(season_episodes["Items"][1]["IndexNumber"], 2);
    let invalid_season = get_json(
        &app,
        &format!("/Shows/{series_id}/Episodes?SeasonId=another-season"),
    )
    .await;
    assert!(invalid_season["Items"].as_array().unwrap().is_empty());
}

/// 播放事件让第二集进入 Resume。
async fn progress_marks_the_episode_resumable(fx: &TvSeasonFilterFixture) {
    let app = fx.app.clone();
    let second_id = fx.second_id.clone();
    let play_session = play_session_for(&app, &second_id, None).await;
    assert_eq!(
        post_event_without_device(&app, "/Sessions/Playing", &second_id, &play_session, 0).await,
        StatusCode::NO_CONTENT
    );
    let progress = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/Sessions/Playing/Progress")
                .header("authorization", "Bearer admin-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "ItemId": second_id.to_string().replace('-', ""),
                        "PositionTicks": 75_000_000,
                        "PlaySessionId": play_session
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(progress.status(), StatusCode::NO_CONTENT);
    let resumed = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume",
    )
    .await;
    let resumed_item = resumed["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["Id"] == second_id.to_string().replace('-', ""))
        .unwrap();
    assert_eq!(resumed_item["Type"], "Episode");
    assert_eq!(
        resumed_item["UserData"]["PlaybackPositionTicks"],
        75_000_000
    );
}

/// 单集/整季已看与 show 收藏如何影响 NextUp 与剧集汇总。
async fn played_and_favorite_marks_update_next_up_and_series_counts(fx: &TvSeasonFilterFixture) {
    let app = fx.app.clone();
    let series_id = fx.series_id.clone();
    let season_one_id = fx.season_one_id.clone();
    let first_id = fx.first_id.clone();
    let second_id = fx.second_id.clone();
    let third_id = fx.third_id.clone();
    let mark_episode = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/UserPlayedItems/{}",
                    first_id.to_string().replace('-', "")
                ))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mark_episode.status(), StatusCode::OK);
    let next_up = get_json(&app, "/Shows/NextUp").await;
    assert_eq!(
        next_up["Items"][0]["Id"],
        second_id.to_string().replace('-', "")
    );

    let favorite_show = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/UserFavoriteItems/{series_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(favorite_show.status(), StatusCode::OK);
    assert_eq!(
        get_json(&app, "/Shows/NextUp").await["Items"][0]["Id"],
        second_id.to_string().replace('-', "")
    );

    let mark_season = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/UserPlayedItems/{season_one_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mark_season.status(), StatusCode::OK);
    let next_up_after_season = get_json(&app, "/Shows/NextUp").await;
    assert_eq!(
        next_up_after_season["Items"][0]["Id"],
        third_id.to_string().replace('-', "")
    );
    let season_episode = get_json(
        &app,
        &format!("/Items/{}", first_id.to_string().replace('-', "")),
    )
    .await;
    assert_eq!(season_episode["UserData"]["Played"], true);
    let series_listing = get_json(&app, "/Items").await;
    let listed_series = series_listing["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["Name"] == "Show")
        .unwrap();
    assert_eq!(listed_series["UserData"]["Played"], false);
    assert_eq!(listed_series["RecursiveItemCount"], 4);
    assert_eq!(listed_series["UserData"]["UnplayedItemCount"], 2);
    let direct_series = get_json(&app, &format!("/Items/{series_id}")).await;
    assert_eq!(
        direct_series["UserData"]["Played"],
        listed_series["UserData"]["Played"]
    );
    assert_eq!(
        direct_series["RecursiveItemCount"],
        listed_series["RecursiveItemCount"]
    );
    assert_eq!(
        direct_series["UserData"]["UnplayedItemCount"],
        listed_series["UserData"]["UnplayedItemCount"]
    );
    let resume_after_season = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume",
    )
    .await;
    assert!(resume_after_season["Items"].as_array().unwrap().is_empty());
}

/// 取消第二集已看并收藏第一季：NextUp/Resume 恢复。
async fn unmarking_the_favorite_season_episode_restores_next_up(fx: &TvSeasonFilterFixture) {
    let app = fx.app.clone();
    let second_id = fx.second_id.clone();
    let season_one_id = fx.season_one_id.clone();
    let unmark_second = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/UserPlayedItems/{}",
                    second_id.to_string().replace('-', "")
                ))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unmark_second.status(), StatusCode::OK);
    assert_eq!(
        get_json(&app, "/Shows/NextUp").await["Items"][0]["Id"],
        second_id.to_string().replace('-', "")
    );
    let direct_unmarked = get_json(
        &app,
        &format!("/Items/{}", second_id.to_string().replace('-', "")),
    )
    .await;
    assert_eq!(direct_unmarked["UserData"]["Played"], false);
    let favorite_season = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/UserFavoriteItems/{season_one_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(favorite_season.status(), StatusCode::OK);
    let after_favorite_detail = get_json(
        &app,
        &format!("/Items/{}", second_id.to_string().replace('-', "")),
    )
    .await;
    assert_eq!(after_favorite_detail["UserData"]["Played"], false);
    assert_eq!(
        get_json(&app, "/Shows/NextUp").await["Items"][0]["Id"],
        second_id.to_string().replace('-', "")
    );
    let resume_after_favorite = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume",
    )
    .await;
    assert!(
        resume_after_favorite["Items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["Id"] == second_id.to_string().replace('-', ""))
    );
}

/// 标记整个 show 已看：NextUp 与 Resume 清空。
async fn marking_the_series_clears_next_up_and_resume(fx: &TvSeasonFilterFixture) {
    let app = fx.app.clone();
    let series_id = fx.series_id.clone();
    let season_two_id = fx.season_two_id.clone();
    let mark_show = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/UserPlayedItems/{series_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mark_show.status(), StatusCode::OK);
    let next_up_after_show = get_json(&app, "/Shows/NextUp").await;
    assert!(next_up_after_show["Items"].as_array().unwrap().is_empty());
    let resume_after_show = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume",
    )
    .await;
    assert!(resume_after_show["Items"].as_array().unwrap().is_empty());

    let favorite_unmarked_season = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/UserFavoriteItems/{season_two_id}"))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(favorite_unmarked_season.status(), StatusCode::OK);
    assert!(
        get_json(&app, "/Shows/NextUp").await["Items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// 收藏一集未看剧集不得凭空产生已看事实。
async fn favoriting_an_unplayed_episode_does_not_invent_played(fx: &TvSeasonFilterFixture) {
    let app = fx.app.clone();
    let tmp = &fx.tmp;
    let show = &fx.show;
    let third_id = fx.third_id.clone();
    let fourth_id = fx.fourth_id.clone();
    let favorite_episode = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/UserFavoriteItems/{}",
                    fourth_id.to_string().replace('-', "")
                ))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(favorite_episode.status(), StatusCode::OK);
    assert!(
        get_json(&app, "/Shows/NextUp").await["Items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let favorited_unplayed_unit = Store::open(tmp.path().join("data"))
        .unwrap()
        .unit_state(
            domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap(),
            show.id,
            2,
            2,
        )
        .unwrap()
        .unwrap();
    assert!(
        !favorited_unplayed_unit.played,
        "favouriting must not invent an explicit played fact"
    );
    assert!(favorited_unplayed_unit.favorite);

    let unmark_episode = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/UserPlayedItems/{}",
                    third_id.to_string().replace('-', "")
                ))
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unmark_episode.status(), StatusCode::OK);
    let next_up_after_unmark = get_json(&app, "/Shows/NextUp").await;
    assert_eq!(
        next_up_after_unmark["Items"][0]["Id"],
        third_id.to_string().replace('-', "")
    );
    let unplayed_override = Store::open(tmp.path().join("data"))
        .unwrap()
        .unit_state(
            domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap(),
            show.id,
            2,
            1,
        )
        .unwrap()
        .unwrap();
    assert!(!unplayed_override.played);
}

#[tokio::test]
async fn tv_season_filter_resume_and_show_played_marks_work() {
    let fx = tv_season_filter_fixture().await;
    episode_listing_reflects_season_filter(&fx).await;
    progress_marks_the_episode_resumable(&fx).await;
    played_and_favorite_marks_update_next_up_and_series_counts(&fx).await;
    unmarking_the_favorite_season_episode_restores_next_up(&fx).await;
    marking_the_series_clears_next_up_and_resume(&fx).await;
    favoriting_an_unplayed_episode_does_not_invent_played(&fx).await;
}

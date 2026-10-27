//! catalog 大类：Resume 的候选收集、去重与分页。
use super::*;

#[tokio::test]
async fn resume_hides_episode_whose_effective_played_comes_from_a_newer_show_mark() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv-resume-effective");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Resume effective",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = new_media(MediaKind::Tv, "Resume effective show");
    store.insert_media(&show).unwrap();
    let episode = insert_episode(&store, &root, show.id, 1, 1);
    let user = store.admin_user_id().unwrap();
    // Raw episode unit is still in progress. A later whole-show mark is the
    // only source of played=true; resume SQL sees only the raw unit.
    store
        .upsert_unit(
            user,
            show.id,
            1,
            1,
            7_500,
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
    let raw = store.user_resumable_units(user).unwrap();
    let raw_played = store
        .unit_state(user, show.id, 1, 1)
        .unwrap()
        .unwrap()
        .played;
    drop(store);
    let episode_id = episode.to_string().replace('-', "");
    let detail = get_json(&app, &format!("/Items/{episode_id}")).await;
    let resumed = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume",
    )
    .await;
    let listed = resumed["Items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["Id"] == episode_id);
    assert!(
        raw.iter()
            .any(|row| row.0 == show.id && row.1 == 1 && row.2 == 1),
        "raw resume query must admit the in-progress episode, got {raw:?}"
    );
    assert!(!raw_played, "raw episode unit must stay played=false");
    assert_eq!(
        detail["UserData"]["Played"], true,
        "effective played must be true"
    );
    assert!(
        !listed,
        "resume must not return an item whose UserData.Played is true"
    );
}

#[tokio::test]
async fn resume_fills_the_page_after_dropping_effectively_played_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv-resume-page");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Resume page",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let user = store.admin_user_id().unwrap();
    let mut expected = Vec::new();
    for index in 0..3 {
        let show = new_media(MediaKind::Tv, &format!("Resume page {index}"));
        store.insert_media(&show).unwrap();
        let episode_number = index + 1;
        let episode = insert_episode(&store, &root, show.id, 1, episode_number);
        let updated_at = 30 - index as i64;
        store
            .upsert_unit(
                user,
                show.id,
                1,
                episode_number as i32,
                7_500,
                Some(false),
                None,
                None,
                None,
                None,
                false,
                updated_at,
            )
            .unwrap();
        if index == 0 {
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
                    updated_at + 1,
                )
                .unwrap();
        } else {
            expected.push(episode.to_string().replace('-', ""));
        }
    }
    drop(store);

    let resumed = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume?Limit=2",
    )
    .await;
    let ids = resumed["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["Id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(resumed["TotalRecordCount"], 2);
    assert_eq!(ids, expected);
    assert!(
        resumed["Items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["UserData"]["Played"] == false)
    );
}

#[tokio::test]
async fn resume_uses_first_eligible_episode_per_series() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv-resume-series-dedup");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Resume dedup",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = new_media(MediaKind::Tv, "Resume dedup show");
    store.insert_media(&show).unwrap();
    let season_one = insert_episode(&store, &root, show.id, 1, 1);
    let season_two = insert_episode(&store, &root, show.id, 2, 1);
    let user = store.admin_user_id().unwrap();
    store
        .upsert_unit(
            user,
            show.id,
            1,
            1,
            7_500,
            Some(false),
            None,
            None,
            None,
            None,
            false,
            30,
        )
        .unwrap();
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
            40,
        )
        .unwrap();
    store
        .upsert_unit(
            user,
            show.id,
            2,
            1,
            7_500,
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

    let resumed = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume?Limit=1",
    )
    .await;
    let items = resumed["Items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["Id"], season_two.to_string().replace('-', ""));
    assert_ne!(items[0]["Id"], season_one.to_string().replace('-', ""));
}

#[tokio::test]
async fn resume_paginates_after_collecting_all_eligible_items() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv-resume-pagination");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "Resume pagination",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let user = store.admin_user_id().unwrap();
    let mut expected = Vec::new();
    for index in 0..3 {
        let show = new_media(MediaKind::Tv, &format!("Resume pagination {index}"));
        store.insert_media(&show).unwrap();
        let episode_root = root.join(format!("show-{index}"));
        std::fs::create_dir_all(&episode_root).unwrap();
        let episode = insert_episode(&store, &episode_root, show.id, 1, 1);
        store
            .upsert_unit(
                user,
                show.id,
                1,
                1,
                7_500,
                Some(false),
                None,
                None,
                None,
                None,
                false,
                30 - index,
            )
            .unwrap();
        expected.push(episode.to_string().replace('-', ""));
    }
    drop(store);

    let resumed = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume?StartIndex=1&Limit=1",
    )
    .await;
    assert_eq!(resumed["TotalRecordCount"], 3);
    assert_eq!(resumed["Items"].as_array().unwrap().len(), 1);
    assert_eq!(resumed["Items"][0]["Id"], expected[1]);
}

#[tokio::test]
async fn resume_lists_only_the_latest_in_progress_episode_per_series() {
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
    let user_id = domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    for episode in 1..=4 {
        insert_episode(&store, &root, show.id, 1, episode);
        store
            .upsert_unit(
                user_id,
                show.id,
                1,
                episode as i32,
                10_000 * episode as i64,
                Some(false),
                None,
                None,
                None,
                None,
                false,
                episode as i64 + 100,
            )
            .unwrap();
    }

    let resumed = get_json(
        &app,
        "/Users/00000000-0000-0000-0000-000000000001/Items/Resume",
    )
    .await;
    assert_eq!(resumed["TotalRecordCount"], 1);
    assert_eq!(resumed["Items"].as_array().unwrap().len(), 1);
    assert_eq!(resumed["Items"][0]["IndexNumber"], 4);
}

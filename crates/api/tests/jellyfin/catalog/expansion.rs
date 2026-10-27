//! catalog 大类：库/folder/SeriesId 作用域下的剧集展开与类型过滤。
use super::*;

#[tokio::test]
async fn library_episode_expansion_respects_nested_and_default_library_ownership() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let parent_root = tmp.path().join("tv");
    let child_root = parent_root.join("child");
    std::fs::create_dir_all(&child_root).unwrap();
    let parent = store
        .create_library(
            MediaKind::Tv,
            "Parent shows",
            &[parent_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let child = store
        .create_library(
            MediaKind::Tv,
            "Child shows",
            &[child_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store.set_default_library(&parent.id).unwrap();
    let show = new_media(MediaKind::Tv, "Nested show");
    store.insert_media(&show).unwrap();
    let parent_episode = insert_episode(&store, &parent_root, show.id, 1, 1);
    let child_episode = insert_episode(&store, &child_root, show.id, 1, 2);
    let outside_root = tmp.path().join("unowned-tv-path");
    std::fs::create_dir_all(&outside_root).unwrap();
    // 额外根挂在默认库（parent）上：该集的路径必须由某个库根拥有。
    store
        .insert_library_root(MediaKind::Tv, outside_root.to_str().unwrap())
        .unwrap();
    let outside_episode = insert_episode(&store, &outside_root, show.id, 1, 3);
    drop(store);

    let child_listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode",
            child.id
        ),
    )
    .await;
    let child_episodes = child_listing["Items"].as_array().unwrap();
    assert_eq!(child_episodes.len(), 1);
    assert_eq!(
        child_episodes[0]["Id"],
        child_episode.to_string().replace('-', "")
    );

    let parent_listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode",
            parent.id
        ),
    )
    .await;
    let parent_episodes = parent_listing["Items"].as_array().unwrap();
    let parent_episode_ids = parent_episodes
        .iter()
        .map(|episode| episode["Id"].as_str().unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(parent_episodes.len(), 2);
    assert!(parent_episode_ids.contains(parent_episode.to_string().replace('-', "").as_str()));
    assert!(parent_episode_ids.contains(outside_episode.to_string().replace('-', "").as_str()));
    assert!(!parent_episode_ids.contains(child_episode.to_string().replace('-', "").as_str()));
}

#[tokio::test]
async fn folder_episode_expansion_stays_within_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root_a = tmp.path().join("tv-a");
    let root_b = tmp.path().join("tv-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[root_a.to_str().unwrap(), root_b.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store.set_default_library(&library.id).unwrap();
    let show = new_media(MediaKind::Tv, "Split show");
    store.insert_media(&show).unwrap();
    let episode_a = insert_episode(&store, &root_a, show.id, 1, 1);
    let episode_b = insert_episode(&store, &root_b, show.id, 1, 2);
    drop(store);

    let folder_id = |root: &Path| {
        format!(
            "folder_{}",
            uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_DNS,
                root.to_string_lossy().as_bytes()
            )
            .simple()
        )
    };
    let folder_listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode",
            folder_id(&root_a)
        ),
    )
    .await;
    let folder_episodes = folder_listing["Items"].as_array().unwrap();
    assert_eq!(folder_episodes.len(), 1);
    assert_eq!(
        folder_episodes[0]["Id"],
        episode_a.to_string().replace('-', "")
    );
    let folder_b_listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode",
            folder_id(&root_b)
        ),
    )
    .await;
    assert_eq!(folder_b_listing["Items"].as_array().unwrap().len(), 1);
    assert_eq!(
        folder_b_listing["Items"][0]["Id"],
        episode_b.to_string().replace('-', "")
    );
    assert_eq!(folder_b_listing["TotalRecordCount"], 1);
    assert_eq!(folder_listing["TotalRecordCount"], 1);
    let all_listing = get_json(&app, "/Items?Recursive=true&IncludeItemTypes=Episode").await;
    let all_episodes = all_listing["Items"].as_array().unwrap();
    assert_eq!(all_episodes.len(), 2);

    let series_listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode",
            show.id
        ),
    )
    .await;
    let series_episodes = series_listing["Items"].as_array().unwrap();
    assert_eq!(series_episodes.len(), 2);
    assert!(
        series_episodes
            .iter()
            .any(|episode| episode["Id"] == episode_b.to_string().replace('-', ""))
    );
}

#[tokio::test]
async fn folder_series_id_returns_only_series_episodes_in_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root_a = tmp.path().join("tv-series-scope-a");
    let root_b = tmp.path().join("tv-series-scope-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            "Series scope shows",
            &[root_a.to_str().unwrap(), root_b.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store.set_default_library(&library.id).unwrap();
    let show = new_media(MediaKind::Tv, "Folder constrained show");
    store.insert_media(&show).unwrap();
    let episode_a = insert_episode(&store, &root_a, show.id, 1, 1);
    insert_episode(&store, &root_b, show.id, 1, 2);
    drop(store);

    let folder_id = format!(
        "folder_{}",
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_DNS,
            root_a.to_string_lossy().as_bytes()
        )
        .simple()
    );
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={folder_id}&SeriesId={}&Recursive=true&IncludeItemTypes=Episode",
            show.id
        ),
    )
    .await;
    let episodes = listing["Items"].as_array().unwrap();
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0]["Id"], episode_a.to_string().replace('-', ""));
    assert_eq!(listing["TotalRecordCount"], 1);
}

#[tokio::test]
async fn series_and_episode_types_return_both_series_and_episodes() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _, _) = app(tmp.path());
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv-series-and-episode");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            "Combined types",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = new_media(MediaKind::Tv, "Combined type show");
    store.insert_media(&show).unwrap();
    let episode = insert_episode(&store, &root, show.id, 1, 1);
    drop(store);

    // This fixture has one visible series row and its one episode row.
    let combined = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series,Episode",
            library.id
        ),
    )
    .await;
    let combined_items = combined["Items"].as_array().unwrap();
    assert_eq!(combined_items.len(), 2);
    assert_eq!(
        combined_items
            .iter()
            .filter(|item| item["Type"] == "Series")
            .count(),
        1
    );
    assert_eq!(
        combined_items
            .iter()
            .filter(|item| item["Type"] == "Episode")
            .count(),
        1
    );
    assert!(
        combined_items
            .iter()
            .any(|item| item["Id"] == work_id(show.id))
    );
    assert!(
        combined_items
            .iter()
            .any(|item| item["Id"] == episode.to_string().replace('-', ""))
    );
    assert_eq!(combined["TotalRecordCount"], 2);

    let episodes_only = get_json(&app, "/Items?Recursive=true&IncludeItemTypes=Episode").await;
    assert_eq!(episodes_only["Items"].as_array().unwrap().len(), 1);
    assert_eq!(
        episodes_only["Items"][0]["Id"],
        episode.to_string().replace('-', "")
    );
}

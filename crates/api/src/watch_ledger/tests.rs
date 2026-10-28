use super::*;

#[test]
fn scan_observation_cannot_publish_into_a_root_created_after_planning() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    let child = root.join("nested");
    std::fs::create_dir_all(&child).unwrap();
    let path = child.join("Movie.2024.mkv");
    std::fs::write(&path, b"movie").unwrap();
    std::fs::write(
        child.join("movie.nfo"),
        "<movie><title>Movie</title></movie>",
    )
    .unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Parent",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let plan = plan_paths(
        &store,
        [library::TransferredFile {
            path: path.clone(),
            identified_release: None,
            nfo_identity: None,
        }],
    )
    .unwrap();
    let observed = plan.observe().unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Child",
            &[child.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let result = observed.record(&store);
    assert!(
        result.is_err(),
        "stale observations must not change Library: {result:?}"
    );
    assert!(
        store
            .file_coverage_by_path(&path.to_string_lossy())
            .unwrap()
            .is_none()
    );
}

#[test]
fn existing_scan_observation_rejects_a_later_nested_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    let child = root.join("nested");
    std::fs::create_dir_all(&child).unwrap();
    let path = child.join("Movie.2024.mkv");
    std::fs::write(&path, b"movie").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Parent",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let input = || {
        [library::TransferredFile {
            path: path.clone(),
            identified_release: None,
            nfo_identity: None,
        }]
    };
    plan_paths(&store, input())
        .unwrap()
        .observe()
        .unwrap()
        .record(&store)
        .unwrap();
    let observed = plan_paths(&store, input()).unwrap().observe().unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Child",
            &[child.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    assert!(observed.record(&store).is_err());
}

#[test]
fn existing_scan_refuses_observations_of_a_replaced_disk_source() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("Movie.2024.mkv");
    std::fs::write(&path, b"movie").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let input = || {
        [library::TransferredFile {
            path: path.clone(),
            identified_release: None,
            nfo_identity: None,
        }]
    };
    plan_paths(&store, input())
        .unwrap()
        .observe()
        .unwrap()
        .record(&store)
        .unwrap();
    let plan = plan_paths(&store, input()).unwrap();
    std::fs::write(&path, b"replacement bytes").unwrap();
    std::fs::write(
        root.join("Movie.2024.nfo"),
        "<movie><plot>Replaced source</plot></movie>",
    )
    .unwrap();
    plan.observe().unwrap().record(&store).unwrap();
    let row = store
        .file_coverage_by_path(path.to_str().unwrap())
        .unwrap()
        .unwrap();
    let detail = store
        .library_item_detail(row.file.library_id, row.file.media_id, "")
        .unwrap()
        .unwrap();
    assert!(
        detail.metadata.overview.value().is_none(),
        "NFO from unregistered replacement source must not publish"
    );
}

#[test]
fn abnormal_episode_range_maps_to_a_single_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("shows");
    let dir = root.join("Xian Ni").join("Season 1");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Xian.Ni.S01E90.mkv");
    std::fs::write(&path, b"ep").unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
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
    let mut parsed = release::parse("Xian.Ni.S01E90.mkv");
    parsed.episode_to = Some(2160);
    let plan = plan_paths(
        &store,
        [library::TransferredFile {
            path,
            identified_release: Some(parsed),
            nfo_identity: None,
        }],
    )
    .unwrap();
    assert_eq!(plan.files.len(), 1);
    assert_eq!(plan.files[0].row.units, vec![UnitKey::new(1, 90)]);
}

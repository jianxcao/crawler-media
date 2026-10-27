//! 物理版本折叠：同一逻辑条目（Work/Unit）的多份文件只出现一次。

use super::*;

async fn listing(app: &axum::Router, uri: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("authorization", "Bearer admin-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json(response).await
}

fn insert_version(
    store: &Store,
    media_id: MediaId,
    path: &Path,
    id: &str,
    unit: (Option<u32>, Option<u32>),
) {
    std::fs::write(path, b"version").unwrap();
    let spec = store::test_support::FixtureFile {
        media_id,
        path: path.display().to_string(),
        season: unit.0,
        episode: unit.1,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        transfer_mode: None,
        source_path: None,
        file_id: Some(LedgerId::from_str(id).unwrap()),
    };
    store::test_support::publish_fixture_file(store, &spec).unwrap();
}

fn router_for(store: Store, dir: &Path) -> axum::Router {
    router(
        ApiState::new(
            store,
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.join("stage"))),
            dir.join("library"),
        )
        .unwrap(),
    )
}

const FIRST: &str = "00000000-0000-0000-0002-000000000001";
const SECOND: &str = "00000000-0000-0000-0002-000000000002";

/// One Movie with two published files; `newer` gets a higher `file_revision`.
fn duplicate_movie(dir: &Path, newer: Option<&str>) -> (axum::Router, MediaId, String) {
    let root = dir.join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(dir.join("data")).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Duplicated".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    insert_version(
        &store,
        media.id,
        &root.join("first.mkv"),
        FIRST,
        (None, None),
    );
    insert_version(
        &store,
        media.id,
        &root.join("second.mkv"),
        SECOND,
        (None, None),
    );
    if let Some(name) = newer {
        let file_id = if name == "first.mkv" { FIRST } else { SECOND };
        rusqlite::Connection::open(dir.join("data/library.db"))
            .unwrap()
            .execute(
                "UPDATE library_files SET file_revision = 5 WHERE file_id = ?1",
                [file_id],
            )
            .unwrap();
    }
    (router_for(store, dir), media.id, library.id)
}

#[tokio::test]
async fn duplicate_physical_versions_collapse_to_one_logical_item() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media_id, library_id) = duplicate_movie(tmp.path(), None);
    let body = listing(
        &app,
        &format!("/Items?ParentId={library_id}&Recursive=true"),
    )
    .await;

    assert_eq!(body["TotalRecordCount"], 1, "{body}");
    let items = body["Items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["Id"], work_id(media_id));
    // Equal revisions: the smaller file id wins, deterministically.
    assert!(
        items[0]["Path"].as_str().unwrap().ends_with("first.mkv"),
        "{body}"
    );
}

#[tokio::test]
async fn newest_physical_revision_is_the_kept_source() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, media_id, library_id) = duplicate_movie(tmp.path(), Some("second.mkv"));
    let body = listing(
        &app,
        &format!("/Items?ParentId={library_id}&Recursive=true"),
    )
    .await;

    assert_eq!(body["TotalRecordCount"], 1, "{body}");
    assert_eq!(body["Items"][0]["Id"], work_id(media_id));
    assert!(
        body["Items"][0]["Path"]
            .as_str()
            .unwrap()
            .ends_with("second.mkv"),
        "播放必须偏向最新版本：{body}"
    );
}

#[tokio::test]
async fn duplicate_episode_versions_collapse_to_one_unit_item() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let library = store
        .create_library(
            MediaKind::Tv,
            "Shows",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let show = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: "Show".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&show).unwrap();
    insert_version(
        &store,
        show.id,
        &root.join("first.mkv"),
        FIRST,
        (Some(1), Some(1)),
    );
    insert_version(
        &store,
        show.id,
        &root.join("second.mkv"),
        SECOND,
        (Some(1), Some(1)),
    );
    let app = router_for(store, tmp.path());
    let body = listing(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode",
            library.id
        ),
    )
    .await;

    assert_eq!(body["TotalRecordCount"], 1, "{body}");
    assert_eq!(body["Items"][0]["Id"], unit_id(show.id, 1, 1));
}

//! 计划偏差⑤（测试盲区）补齐：粗粒度路由的行为测试 —— `/Items/Counts`、`/Items/Latest`、
//! `/Shows/{id}/Seasons`、`/Genres`、`/Studios`（外加 `/UserViews` 一条粗断言），
//! 以及 `Recursive` 参数在「非库根 parent」上的现状判定。
//!
//! 断言只用 HTTP 响应（公开缝）。`/Items/Latest` 另加**工作量上界**断言：页内重元数据
//! 只覆盖返回页 ⇒ 现场水合计数等于返回条目数；旧实现整库取快照再内存切片，计数恒为 0。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use tower::ServiceExt;

/// NFO sidecar：只在 `publish_fixture_file` 发布那一刻被观察并采纳成库内事实。
const COARSE_NFO: &str = "<movie><genre>Drama</genre><studio>Studio X</studio></movie>";

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

struct Coarse {
    _dir: tempfile::TempDir,
    state: ApiState,
    heavy: Arc<AtomicUsize>,
    /// `Target Show` 的 `w1_…` 公开 id（`/Shows/{id}/Seasons` 用）。
    series: String,
    /// Movies 库的 library id（`/Items/Latest` 用库根收窄候选集）。
    movies_library: String,
}

/// 一个 Movie 库（4 部电影，`Movie 03` 已看完）+ 一个 Tv 库（1 部剧、2 季各 2 集）。
fn coarse() -> Coarse {
    let dir = tempfile::tempdir().unwrap();
    let heavy = Arc::new(AtomicUsize::new(0));
    let candidates = Arc::new(AtomicUsize::new(0));
    let store = Store::open(dir.path().join("data")).unwrap();
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap()
    .with_jellyfin_work_counters(candidates, heavy.clone());
    let movies_root = dir.path().join("movies");
    let shows_root = dir.path().join("shows");
    std::fs::create_dir_all(&movies_root).unwrap();
    std::fs::create_dir_all(&shows_root).unwrap();
    let user = state
        .store()
        .lock()
        .user_id_by_token("management-secret")
        .unwrap()
        .unwrap();
    let media = {
        let guard = state.store();
        let store = guard.lock();
        let library = store
            .create_library(
                MediaKind::Movie,
                "Movies",
                &[movies_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        for index in 0..4 {
            let title = format!("Movie {index:02}");
            let movie = insert_movie(&store, &movies_root, &title);
            if index == 3 {
                store
                    .upsert_unit(
                        user,
                        movie,
                        -1,
                        -1,
                        0,
                        Some(true),
                        None,
                        Some(600_000),
                        None,
                        None,
                        false,
                        100,
                    )
                    .unwrap();
            }
        }
        store
            .create_library(
                MediaKind::Tv,
                "Shows",
                &[shows_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let series = insert_series(&store, &shows_root, "Target Show");
        (library.id, series)
    };
    Coarse {
        _dir: dir,
        state,
        heavy,
        series: domain::TypedItemId::Work(media.1).to_wire_string(),
        movies_library: media.0,
    }
}

fn insert_movie(store: &Store, root: &Path, title: &str) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
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
    let path = root.join(format!("{title}.mkv"));
    std::fs::write(&path, b"payload").unwrap();
    std::fs::write(root.join(format!("{title}.nfo")), COARSE_NFO).unwrap();
    publish_fixture(store, media.id, &path, None, None);
    media.id
}

/// 2 季 × 2 集；没有任何一集看过（`/Shows/{id}/Seasons` 的未看计数因此可预期）。
fn insert_series(store: &Store, root: &Path, title: &str) -> MediaId {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Tv,
        title: title.into(),
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
    let show_dir = root.join(title);
    std::fs::create_dir_all(&show_dir).unwrap();
    for season in 1..=2u32 {
        for episode in 1..=2u32 {
            let path = show_dir.join(format!("S{season:02}E{episode:02}.mkv"));
            std::fs::write(&path, b"payload").unwrap();
            publish_fixture(store, media.id, &path, Some(season), Some(episode));
        }
    }
    media.id
}

/// 走真实发布路径：`publish_fixture_file` 内部先 `observe_nfo_inputs` 再 `publish_files`，
/// 所以调用前落盘的 `.nfo` 与旧的 ledger 插入同一时刻被观察。
fn publish_fixture(
    store: &Store,
    media: MediaId,
    path: &Path,
    season: Option<u32>,
    episode: Option<u32>,
) -> LedgerId {
    let spec = store::test_support::FixtureFile {
        media_id: media,
        path: path.display().to_string(),
        season,
        episode,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        ..Default::default()
    };
    store::test_support::publish_fixture_file(store, &spec)
        .unwrap()
        .file_id
}

async fn get_json(app: &axum::Router, uri: &str) -> Value {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    body
}

fn items(body: &Value) -> &Vec<Value> {
    body["Items"].as_array().expect("Items array")
}

/// `/Items/Latest` 每个返回条目的 `UserData.Played`；响应是**裸数组**（无 `TotalRecordCount`）。
fn latest_played(body: &Value) -> Vec<bool> {
    body.as_array()
        .expect("Latest 是裸数组")
        .iter()
        .map(|item| item["UserData"]["Played"].as_bool().unwrap_or(false))
        .collect()
}

/// `/Items/Latest`：与返回页无关的整库快照不允许存在 —— 页内水合次数必须等于返回条目数。
#[tokio::test]
async fn latest_hydrates_only_the_requested_page() {
    let fx = coarse();
    let app = router(fx.state.clone());
    let uri = format!(
        "/Users/Me/Items/Latest?ParentId={}&Limit=3",
        fx.movies_library
    );
    let body = get_json(&app, &uri).await;
    let listed = body.as_array().expect("Latest 是裸数组");
    assert_eq!(listed.len(), 3, "{uri}: {body}");
    assert_eq!(
        latest_played(&body),
        vec![false, false, false],
        "最近入库：未看完在前（已看完的 `Movie 03` 沉底）"
    );
    assert_eq!(
        fx.heavy.load(Ordering::SeqCst),
        3,
        "页内重元数据只应覆盖返回页（旧实现整库快照 ⇒ 0）"
    );
}

/// `/Items/Latest` 跨段边界：未看完段耗尽后接已看完段，偏移量在段间折算。
#[tokio::test]
async fn latest_crosses_into_the_played_segment() {
    let fx = coarse();
    let app = router(fx.state.clone());
    let uri = format!(
        "/Users/Me/Items/Latest?ParentId={}&StartIndex=3&Limit=2",
        fx.movies_library
    );
    let body = get_json(&app, &uri).await;
    let listed = body.as_array().expect("Latest 是裸数组");
    assert_eq!(listed.len(), 1, "{uri}: {body}");
    assert_eq!(latest_played(&body), vec![true], "{uri}: {body}");
    assert_eq!(
        fx.heavy.load(Ordering::SeqCst),
        1,
        "跨段页只水合返回的那一条"
    );
}

/// `/Items/Counts`：字段形状 + 粗粒度计数（`MovieCount` 是可见电影行数）。
#[tokio::test]
async fn counts_reports_kind_totals() {
    let fx = coarse();
    let app = router(fx.state.clone());
    let body = get_json(&app, "/Items/Counts").await;
    println!("counts -> {body}");
    assert_eq!(body["MovieCount"], 4, "{body}");
    for key in ["SeriesCount", "EpisodeCount", "ItemCount"] {
        assert!(body[key].is_number(), "{key} 必须是数字: {body}");
    }
    assert!(
        body["ItemCount"].as_u64().unwrap() >= body["MovieCount"].as_u64().unwrap(),
        "{body}"
    );
}

/// `/Shows/{id}/Seasons`：每季一条、带未看计数/播放百分比，`TotalRecordCount` 是季数。
#[tokio::test]
async fn seasons_report_per_season_user_data() {
    let fx = coarse();
    let app = router(fx.state.clone());
    let uri = format!("/Shows/{}/Seasons", fx.series);
    let body = get_json(&app, &uri).await;
    let listed = items(&body);
    assert_eq!(listed.len(), 2, "{uri}: {body}");
    assert_eq!(body["TotalRecordCount"], 2, "{uri}: {body}");
    for season in listed {
        assert_eq!(season["Type"], "Season", "{season}");
        assert_eq!(season["SeriesId"], Value::String(fx.series.clone()));
        assert_eq!(season["UserData"]["UnplayedItemCount"], 2, "{season}");
        assert_eq!(season["UserData"]["PlayedPercentage"], 0.0, "{season}");
        assert_eq!(season["UserData"]["Played"], false, "{season}");
        assert!(season["ImageTags"]["Primary"].is_string(), "{season}");
    }
}

/// `/Genres`：可见库的 genre 聚合，`ChildCount` 是条目数；分页只切 `Items`，
/// `TotalRecordCount` 是**未分页**的总数。
#[tokio::test]
async fn genres_list_visible_genres_with_child_count() {
    let fx = coarse();
    let app = router(fx.state.clone());
    let body = get_json(&app, "/Genres").await;
    let listed = items(&body);
    assert!(!listed.is_empty(), "{body}");
    let drama = listed
        .iter()
        .find(|genre| genre["Name"] == "Drama")
        .unwrap_or_else(|| panic!("Drama 应可见: {body}"));
    assert_eq!(drama["Type"], "Genre", "{drama}");
    assert_eq!(drama["IsFolder"], true, "{drama}");
    assert!(drama["ChildCount"].as_u64().unwrap() >= 4, "{drama}");
    assert_eq!(body["TotalRecordCount"], listed.len() as u64, "{body}");
    let paged = get_json(&app, "/Genres?Limit=1").await;
    assert_eq!(items(&paged).len(), 1, "{paged}");
    assert_eq!(
        paged["TotalRecordCount"], body["TotalRecordCount"],
        "{paged}"
    );
}

/// `/Studios`：与 `/Genres` 同形，`Type` 是 `Studio`。
#[tokio::test]
async fn studios_list_visible_studios_with_child_count() {
    let fx = coarse();
    let app = router(fx.state.clone());
    let body = get_json(&app, "/Studios").await;
    let listed = items(&body);
    let studio = listed
        .iter()
        .find(|studio| studio["Name"] == "Studio X")
        .unwrap_or_else(|| panic!("Studio X 应可见: {body}"));
    assert_eq!(studio["Type"], "Studio", "{studio}");
    assert_eq!(studio["IsFolder"], true, "{studio}");
    assert!(studio["ChildCount"].as_u64().unwrap() >= 4, "{studio}");
    assert_eq!(body["TotalRecordCount"], listed.len() as u64, "{body}");
    let paged = get_json(&app, "/Studios?Limit=1").await;
    assert_eq!(items(&paged).len(), 1, "{paged}");
    assert_eq!(
        paged["TotalRecordCount"], body["TotalRecordCount"],
        "{paged}"
    );
}

/// `/Users/{id}/Views`：只列库，不扫条目；`/UserViews` 是同一条路由的别名。
#[tokio::test]
async fn views_list_libraries_without_item_scan() {
    let fx = coarse();
    let app = router(fx.state.clone());
    for uri in ["/Users/Me/Views", "/UserViews"] {
        let body = get_json(&app, uri).await;
        let listed = items(&body);
        assert!(!listed.is_empty(), "{uri}: {body}");
        assert_eq!(body["TotalRecordCount"], listed.len() as u64, "{uri}");
        for view in listed {
            assert!(view["Id"].is_string(), "{uri}: {view}");
            assert!(view["Name"].is_string(), "{uri}: {view}");
            assert!(view["CollectionType"].is_string(), "{uri}: {view}");
        }
    }
    assert_eq!(
        fx.heavy.load(Ordering::SeqCst),
        0,
        "列库不应触发任何条目水合"
    );
}

/// `Recursive` 现状判定（Task 11 步骤 3 相关）：库根 parent 会按 `Recursive` 改走
/// 「root 目录列表」；**非库根 parent** 上 `Recursive=false` 与非递归语义无关。
#[tokio::test]
async fn recursive_flag_is_inert_for_non_library_parents() {
    let fx = coarse();
    let app = router(fx.state.clone());
    let recursive = get_json(&app, "/Items?Recursive=true&Limit=50").await;
    let flat = get_json(&app, "/Items?Recursive=false&Limit=50").await;
    assert_eq!(
        recursive, flat,
        "无 ParentId 时 Recursive=false 与 true 完全同体（store 未消费该字段）"
    );
    let series_true = get_json(
        &app,
        &format!("/Items?ParentId={}&Recursive=true&Limit=50", fx.series),
    )
    .await;
    let series_false = get_json(
        &app,
        &format!("/Items?ParentId={}&Recursive=false&Limit=50", fx.series),
    )
    .await;
    println!("Recursive=true  ParentId=<series> -> {series_true}");
    println!("Recursive=false ParentId=<series> -> {series_false}");
    assert_eq!(
        series_true, series_false,
        "非库根 parent：Recursive=false 仍是递归结果"
    );
    let root_true = get_json(
        &app,
        &format!("/Items?ParentId={}&Recursive=true", fx.movies_library),
    )
    .await;
    let root_false = get_json(
        &app,
        &format!("/Items?ParentId={}&Recursive=false", fx.movies_library),
    )
    .await;
    println!("Recursive=true  ParentId=<library> -> {root_true}");
    println!("Recursive=false ParentId=<library> -> {root_false}");
    assert_eq!(items(&root_false)[0]["Type"], "Folder", "{root_false}");
    assert_eq!(items(&root_true)[0]["Type"], "Movie", "{root_true}");
}

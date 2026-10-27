use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use api::file_stat_read::FileStatRead;
use api::media_server_provider::ApiServerProvider;
use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::provider::MediaServerProvider;
use serde_json::Value;
use store::test_support::{FixtureFile, publish_fixture_file};
use tower::ServiceExt;
struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn seed(path: &Path, unrelated: usize) -> (ApiState, domain::UserId, String) {
    let store = Store::open(path.join("data")).unwrap();
    // 目标行放在**非默认**库里：默认库的 `NOT (...)` 回退按语义必须扫全表，
    // 只有非默认库的作用域读才能证明「读不随无关行增长」。
    let target_root = path.join("target-root");
    std::fs::create_dir_all(&target_root).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "Target",
            &[target_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let parent_root = library
        .root_paths
        .first()
        .cloned()
        .unwrap_or_else(|| path.join("media"));
    std::fs::create_dir_all(&parent_root).unwrap();
    let target = parent_root.join("target.mkv");
    std::fs::write(&target, b"target").unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Target".into(),
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
    let spec = FixtureFile {
        media_id: media.id,
        path: target.display().to_string(),
        season: None,
        episode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        file_id: Some(LedgerId::new()),
        ..Default::default()
    };
    publish_fixture_file(&store, &spec).unwrap();
    for index in 0..unrelated {
        let outside = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: format!("Outside {index}"),
            year: None,
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&outside).unwrap();
        let unrelated_path = path
            .join("data/library/movies")
            .join(format!("unrelated-{index}.mkv"));
        std::fs::create_dir_all(unrelated_path.parent().unwrap()).unwrap();
        std::fs::write(&unrelated_path, b"payload").unwrap();
        let outside_spec = FixtureFile {
            media_id: outside.id,
            path: unrelated_path.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
            file_id: Some(LedgerId::new()),
            ..Default::default()
        };
        publish_fixture_file(&store, &outside_spec).unwrap();
    }
    let state = ApiState::new(
        store,
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(path.join("stage"))),
        path.join("library"),
    )
    .unwrap();
    let user = state
        .store()
        .lock()
        .user_id_by_token("management-secret")
        .unwrap()
        .unwrap();
    (state, user, library.id)
}

#[tokio::test]
async fn parent_scope_returns_same_protocol_items_with_unrelated_rows_present() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, user_id, parent_id) = seed(tmp.path(), 40);
    let provider = ApiServerProvider::new(state);
    let items = provider
        .list_visible_items(user_id, Some(&parent_id))
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].media.title, "Target");
}

#[tokio::test]
async fn parent_scope_protocol_json_is_stable_with_unrelated_growth() {
    let small = tempfile::tempdir().unwrap();
    let (small_state, _, small_parent) = seed(small.path(), 0);
    let large = tempfile::tempdir().unwrap();
    let (large_state, _, large_parent) = seed(large.path(), 100);
    let mut small_json = parent_items_json(small_state, &small_parent).await;
    let mut large_json = parent_items_json(large_state, &large_parent).await;
    for response in [&mut small_json, &mut large_json] {
        let item = &mut response["Items"][0];
        item["Id"] = Value::String("target-item".into());
        item["Path"] = Value::String("target-path".into());
        item["ParentId"] = Value::String("target-library".into());
        item["DateCreated"] = Value::Null;
        item["UserData"]["ItemId"] = Value::String("target-item".into());
        item["UserData"]["Key"] = Value::String("target-item".into());
    }
    assert!(
        small_json["Items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["Name"] == "Target"))
    );
    assert!(
        large_json["Items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["Name"] == "Target"))
    );
    assert_eq!(small_json, large_json);
}

async fn parent_items_json(state: ApiState, parent_id: &str) -> Value {
    let request = Request::builder()
        .method("GET")
        .uri(format!("/Items?ParentId={parent_id}"))
        .header("authorization", "Bearer management-secret")
        .body(Body::empty())
        .unwrap();
    let response = router(state).oneshot(request).await.unwrap();
    assert!(response.status().is_success());
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn fixed_parent_sql_candidate_work_ignores_unrelated_rows() {
    let small = tempfile::tempdir().unwrap();
    let (small_state, _, _) = seed(small.path(), 0);
    let small_store = small_state.store();
    let small_guard = small_store.lock();
    let ownership = small_guard.library_ownership().unwrap();
    let parent_id = ownership
        .libraries()
        .iter()
        .find(|library| library.name == "Target")
        .expect("target library")
        .id
        .clone();
    let (small_rows, small_steps) = small_guard
        .jellyfin_ledger_candidates_with_vm_steps(&ownership, &parent_id)
        .unwrap();
    assert_eq!(small_rows.len(), 1);

    let large = tempfile::tempdir().unwrap();
    let (large_state, _, _) = seed(large.path(), 300);
    let large_store = large_state.store();
    let large_guard = large_store.lock();
    let large_ownership = large_guard.library_ownership().unwrap();
    // 大库必须用**它自己**的库 id：否则 scoped 读会走 "该库不在本 ownership" 的
    // 提前返回，测出 0 步，断言恒真（测不到退化）。
    let large_parent_id = large_ownership
        .libraries()
        .iter()
        .find(|library| library.name == "Target")
        .expect("target library")
        .id
        .clone();
    let (large_rows, large_steps) = large_guard
        .jellyfin_ledger_candidates_with_vm_steps(&large_ownership, &large_parent_id)
        .unwrap();
    assert_eq!(large_rows.len(), 1);
    assert!(
        small_steps > 0,
        "fixture read did no DB work: small={small_steps}"
    );
    assert!(
        large_steps <= small_steps * 2,
        "scoped DB work grew with unrelated rows: small={small_steps}, large={large_steps}"
    );
}

// ---------- 选页/水合工作量上界（Task 11 收口） ----------
//
// 这里是**工作量上界**证据，不是行为断言：`ScaleFiles::metadata` 统计逐行现场
// stat 次数，`jellyfin_candidates` / `jellyfin_heavy_items` 统计**生产路径**的
// 候选读行数与重元数据条数。行为等价性由协议 JSON（total/顺序/字段）断言。

/// 计数浏览文件 seam：`metadata` 是逐行现场 stat 的次数。
#[derive(Default)]
struct ScaleFiles {
    metadata: AtomicUsize,
    nfo: AtomicUsize,
}

impl ScaleFiles {
    fn metadata_reads(&self) -> usize {
        self.metadata.load(Ordering::SeqCst)
    }

    fn nfo_reads(&self) -> usize {
        self.nfo.load(Ordering::SeqCst)
    }
}

impl FileStatRead for ScaleFiles {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        self.nfo.fetch_add(1, Ordering::SeqCst);
        library::read_nfo(path)
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        self.metadata.fetch_add(1, Ordering::SeqCst);
        std::fs::metadata(path)
    }
}

struct ScaleTv {
    _dir: tempfile::TempDir,
    state: ApiState,
    files: Arc<ScaleFiles>,
    candidates: Arc<AtomicUsize>,
    heavy: Arc<AtomicUsize>,
    series: String,
    _other: domain::MediaId,
}

/// TV fixture：目标剧 `target` 集在库 `Shows`，另一部剧 `other` 集在库 `Other`。
fn tv_scale(target: usize, other: usize) -> ScaleTv {
    let dir = tempfile::tempdir().unwrap();
    let files = Arc::new(ScaleFiles::default());
    let candidates = Arc::new(AtomicUsize::new(0));
    let heavy = Arc::new(AtomicUsize::new(0));
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
    .with_file_stat_read(files.clone())
    .with_jellyfin_work_counters(candidates.clone(), heavy.clone());
    let shows_root = dir.path().join("shows");
    std::fs::create_dir_all(&shows_root).unwrap();
    let other_root = dir.path().join("other");
    std::fs::create_dir_all(&other_root).unwrap();
    let series_media = seed_tv_library(&state, "Shows", &shows_root, target, "Target Show");
    let other_media = seed_tv_library(&state, "Other", &other_root, other, "Other Show");
    ScaleTv {
        _dir: dir,
        state,
        files,
        candidates,
        heavy,
        series: domain::TypedItemId::Work(series_media).to_wire_string(),
        _other: other_media,
    }
}

fn seed_tv_library(
    state: &ApiState,
    name: &str,
    root: &Path,
    episodes: usize,
    title: &str,
) -> domain::MediaId {
    let guard = state.store();
    let store = guard.lock();
    store
        .create_library(
            MediaKind::Tv,
            name,
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
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
    for episode in 1..=episodes {
        let path = show_dir.join(format!("S01E{episode:02}.mkv"));
        std::fs::write(&path, b"payload").unwrap();
        let ep_spec = FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            season: Some(1),
            episode: Some(episode as u32),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
            file_id: Some(LedgerId::new()),
            ..Default::default()
        };
        publish_fixture_file(&store, &ep_spec).unwrap();
    }
    media.id
}

async fn scale_get_json(app: &axum::Router, uri: &str) -> Value {
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

/// `/Shows/{id}/Episodes`：`total` 是**筛后计数**，水合/现场 stat 只覆盖返回页。
#[tokio::test]
async fn show_episodes_hydrates_only_the_requested_page() {
    let fx = tv_scale(40, 40);
    let app = router(fx.state.clone());
    let uri = format!("/Shows/{}/Episodes?Limit=2", fx.series);
    let body = scale_get_json(&app, &uri).await;
    assert_eq!(body["TotalRecordCount"], 40, "{body}");
    assert_eq!(body["Items"].as_array().map(Vec::len), Some(2), "{body}");
    // 工作量上界：整库 40 集，只允许水合返回页那 2 条。
    assert_eq!(
        fx.heavy.load(Ordering::SeqCst),
        2,
        "页内水合次数应等于返回页条目数（旧路径整库水合，heavy=0）：heavy={}",
        fx.heavy.load(Ordering::SeqCst)
    );
    assert!(
        fx.files.metadata_reads() <= 2 * 2 + 4,
        "现场 stat 覆盖了非返回页：stat={} nfo={}",
        fx.files.metadata_reads(),
        fx.files.nfo_reads()
    );
    assert!(
        fx.candidates.load(Ordering::SeqCst) > 0,
        "选页没走 SQL 候选读：候选={}",
        fx.candidates.load(Ordering::SeqCst)
    );

    fx.heavy.store(0, Ordering::SeqCst);
    let second = scale_get_json(&app, &format!("{uri}&StartIndex=38")).await;
    assert_eq!(second["TotalRecordCount"], 40, "{second}");
    assert_eq!(
        second["Items"].as_array().map(Vec::len),
        Some(2),
        "{second}"
    );
    assert_eq!(
        fx.heavy.load(Ordering::SeqCst),
        2,
        "非零 offset 的页内水合应只覆盖返回页：heavy={}",
        fx.heavy.load(Ordering::SeqCst)
    );
}

struct ScaleResume {
    _dir: tempfile::TempDir,
    state: ApiState,
    files: Arc<ScaleFiles>,
    candidates: Arc<AtomicUsize>,
    heavy: Arc<AtomicUsize>,
}

/// Movie fixture：`in_progress` 部有进度的剧集行 + `played` 部已看完。
fn resume_scale(in_progress: usize, played: usize) -> ScaleResume {
    let dir = tempfile::tempdir().unwrap();
    let files = Arc::new(ScaleFiles::default());
    let candidates = Arc::new(AtomicUsize::new(0));
    let heavy = Arc::new(AtomicUsize::new(0));
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
    .with_file_stat_read(files.clone())
    .with_jellyfin_work_counters(candidates.clone(), heavy.clone());
    let root = dir.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let mut media_ids = Vec::new();
    {
        let guard = state.store();
        let store = guard.lock();
        store
            .create_library(
                MediaKind::Movie,
                "Resume",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        for index in 0..(in_progress + played) {
            let media = Media {
                id: MediaId::new(),
                kind: MediaKind::Movie,
                title: format!("Resume {index:02}"),
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
            let path = root.join(format!("resume-{index:02}.mkv"));
            std::fs::write(&path, b"payload").unwrap();
            let movie_spec = FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                season: None,
                episode: None,
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                filter_score: None,
                file_id: Some(LedgerId::new()),
                ..Default::default()
            };
            publish_fixture_file(&store, &movie_spec).unwrap();
            media_ids.push(media.id);
        }
    }
    let user = state
        .store()
        .lock()
        .user_id_by_token("management-secret")
        .unwrap()
        .unwrap();
    {
        let guard = state.store();
        let store = guard.lock();
        for (index, media_id) in media_ids.iter().enumerate().take(in_progress) {
            store
                .upsert_unit(
                    user,
                    *media_id,
                    -1,
                    -1,
                    30_000,
                    Some(false),
                    None,
                    Some(600_000),
                    None,
                    None,
                    false,
                    10 + index as i64,
                )
                .unwrap();
        }
        for media_id in media_ids.iter().skip(in_progress) {
            store
                .upsert_unit(
                    user,
                    *media_id,
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
    ScaleResume {
        _dir: dir,
        state,
        files,
        candidates,
        heavy,
    }
}

/// `/Users/Me/Items/Resume`：`total` 是全量续播计数，水合只覆盖返回页。
#[tokio::test]
async fn resume_hydrates_only_the_requested_page() {
    let fx = resume_scale(40, 1);
    let app = router(fx.state.clone());
    let body = scale_get_json(&app, "/Users/Me/Items/Resume?Limit=2").await;
    assert_eq!(body["TotalRecordCount"], 40, "{body}");
    assert_eq!(body["Items"].as_array().map(Vec::len), Some(2), "{body}");
    // 顺序合同：最近观看在前，已看完的那条不出现。
    assert_eq!(body["Items"][0]["Name"], "Resume 39", "{body}");
    assert_eq!(body["Items"][1]["Name"], "Resume 38", "{body}");
    // 工作量上界：40 条候选，只允许水合返回页那 2 条。
    assert_eq!(
        fx.heavy.load(Ordering::SeqCst),
        2,
        "页内水合次数应等于返回页条目数（旧路径整库水合，heavy=0）：heavy={}",
        fx.heavy.load(Ordering::SeqCst)
    );
    assert!(
        fx.files.metadata_reads() <= 2 * 2 + 4,
        "现场 stat 覆盖了非返回页：stat={} nfo={}",
        fx.files.metadata_reads(),
        fx.files.nfo_reads()
    );
    assert!(
        fx.candidates.load(Ordering::SeqCst) > 0,
        "续播选页没走 SQL 候选读：候选={}",
        fx.candidates.load(Ordering::SeqCst)
    );

    fx.heavy.store(0, Ordering::SeqCst);
    let second = scale_get_json(&app, "/Users/Me/Items/Resume?Limit=3&StartIndex=2").await;
    assert_eq!(second["TotalRecordCount"], 40, "{second}");
    assert_eq!(
        second["Items"].as_array().map(Vec::len),
        Some(3),
        "{second}"
    );
    assert_eq!(second["Items"][0]["Name"], "Resume 37", "{second}");
    assert_eq!(
        fx.heavy.load(Ordering::SeqCst),
        3,
        "非零 offset 的页内水合应只覆盖返回页：heavy={}",
        fx.heavy.load(Ordering::SeqCst)
    );
}

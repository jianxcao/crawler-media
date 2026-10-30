//! A3：Jellyfin 先选择后重元数据、NextUp 一次聚合的 API 级证据。
//!
//! 覆盖 SCOPE-6-01..04 的回归，以及分页/分集展开/folder/unknown parent/排序 ties/
//! StartIndex/最新默认播放状态等既有组合的等价性。
//!
//! 工作量用注入的 [`FileStatRead`] 计数：`external_subtitles` 每次重元数据构建
//! 调用一次（tracks/chapters/NFO/stat 都在同一处），`metadata` 是 stat 次数。断言
//! 只用公开协议 JSON + 这份 seam，不依赖实现内部类型。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use api::file_stat_read::FileStatRead;
use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::Value;
use tower::ServiceExt;

const TOKEN: &str = "management-secret";

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

type Hook = Box<dyn Fn() + Send + Sync>;

/// 计数浏览文件 seam：重元数据构建（NFO/stat/字幕发现）必须只发生在选中的页上。
#[derive(Default)]
struct CountingFiles {
    metadata: AtomicUsize,
    subtitles: AtomicUsize,
    nfo: AtomicUsize,
    on_first_io: parking_lot::Mutex<Option<Hook>>,
}

impl CountingFiles {
    fn subtitles(&self) -> usize {
        self.subtitles.load(Ordering::SeqCst)
    }

    fn metadata(&self) -> usize {
        self.metadata.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        self.metadata.store(0, Ordering::SeqCst);
        self.subtitles.store(0, Ordering::SeqCst);
        self.nfo.store(0, Ordering::SeqCst);
    }

    /// 第一次文件 IO 时执行一次（模拟请求进行中的存储故障）。
    fn fail_store_on_first_io(&self, hook: Hook) {
        *self.on_first_io.lock() = Some(hook);
    }

    fn fire_once(&self) {
        let hook = self.on_first_io.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }
}

impl FileStatRead for CountingFiles {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        self.nfo.fetch_add(1, Ordering::SeqCst);
        library::read_nfo(path)
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        self.fire_once();
        self.metadata.fetch_add(1, Ordering::SeqCst);
        std::fs::metadata(path)
    }

    fn external_subtitles(&self, path: &Path) -> Result<Vec<library::SubtitleTrack>, String> {
        self.fire_once();
        self.subtitles.fetch_add(1, Ordering::SeqCst);
        library::external_subtitle_tracks(path).map_err(|error| error.to_string())
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    state: ApiState,
    files: Arc<CountingFiles>,
    library_id: String,
    roots: Vec<PathBuf>,
}

/// Fail subsequent heavy reads on the actual pool without changing its schema.
/// The current request must keep rendering the facts captured before file IO.
fn fail_file_details_reads_on_first_io(fixture: &Fixture) -> Arc<AtomicUsize> {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    let failing = Arc::new(AtomicBool::new(false));
    let rejected = Arc::new(AtomicUsize::new(0));
    let pool = fixture.state.store().reader_pool(store::Db::Library);
    let mut leases = (0..8)
        .map(|_| pool.try_checkout().unwrap())
        .collect::<Vec<_>>();
    for lease in &mut leases {
        let failing = failing.clone();
        let rejected = rejected.clone();
        lease
            .with_connection(
                &store::QueryBudget {
                    deadline: Instant::now() + Duration::from_secs(5),
                    cancelled: Arc::new(AtomicBool::new(false)),
                },
                |conn| {
                    conn.authorizer(Some(move |ctx: AuthContext<'_>| {
                        if failing.load(Ordering::SeqCst)
                            && matches!(
                                ctx.action,
                                AuthAction::Read {
                                    table_name: "file_details",
                                    ..
                                }
                            )
                        {
                            rejected.fetch_add(1, Ordering::SeqCst);
                            Authorization::Deny
                        } else {
                            Authorization::Allow
                        }
                    }));
                    Ok(())
                },
            )
            .unwrap();
    }
    drop(leases);
    fixture.files.fail_store_on_first_io(Box::new(move || {
        failing.store(true, Ordering::SeqCst);
    }));
    rejected
}

fn new_state(dir: &Path, files: Arc<CountingFiles>) -> ApiState {
    let store = Store::open(dir.join("data")).unwrap();
    ApiState::new(
        store,
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.join("stage"))),
        dir.join("library"),
    )
    .unwrap()
    .with_file_stat_read(files)
}

fn new_media(store: &Store, title: &str) -> MediaId {
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
    media.id
}

fn insert_episode(
    store: &Store,
    dir: &Path,
    media: MediaId,
    season: u32,
    episode: u32,
) -> LedgerId {
    let path = dir.join(format!("{season:02}x{episode:02}.mkv"));
    std::fs::write(&path, b"episode").unwrap();
    let spec = store::test_support::FixtureFile {
        media_id: media,
        path: path.display().to_string(),
        season: Some(season),
        episode: Some(episode),
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        ..Default::default()
    };
    store::test_support::publish_fixture_file(store, &spec)
        .unwrap()
        .file_id
}

/// TV 库：`shows` 部剧、每部 `episodes` 集，全部挂在同一个根下。
fn tv_fixture(shows: usize, episodes: usize) -> Fixture {
    tv_fixture_seeded(shows, episodes, |_| {})
}

/// 同 [`tv_fixture`]，但每部剧的 sidecar 由 `seed` 在 ledger 行发布**之前**写下。
///
/// NFO 观测只在发布时采集（Task 8），`publish_fixture_file` 之后写下的 sidecar 到不了库内快照，
/// 因此断言页内 genre/studio 的 fixture 必须先把 NFO 放好。
fn tv_fixture_seeded(shows: usize, episodes: usize, seed: impl Fn(&Path)) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let files = Arc::new(CountingFiles::default());
    let state = new_state(dir.path(), files.clone());
    let root = dir.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let library_id = {
        let store = state.store();
        let store = store.as_ref();
        store
            .create_library(
                MediaKind::Tv,
                "Shows",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id
    };
    {
        let store = state.store();
        let store = store.as_ref();
        for show in 0..shows {
            let media_id = new_media(store, &format!("Show{show:03}"));
            let show_dir = root.join(format!("Show{show:03}"));
            std::fs::create_dir_all(&show_dir).unwrap();
            seed(&show_dir);
            for episode in 1..=episodes {
                insert_episode(store, &show_dir, media_id, 1, episode as u32);
            }
        }
    }
    Fixture {
        dir,
        state,
        files,
        library_id,
        roots: vec![root],
    }
}

/// 按标题找 Media（测试 fixture 里标题唯一）。
fn media_for_title(state: &ApiState, title: &str) -> MediaId {
    let store = state.store();
    let store = store.as_ref();
    for row in store.list_file_coverage().unwrap() {
        let Some(media) = store.get_media(row.file.media_id).unwrap() else {
            continue;
        };
        if media.title == title {
            return media.id;
        }
    }
    panic!("media {title} not found");
}

/// 媒体 id 的紧凑拼写：`ParentId` 的既有内部写法。
fn media_id_for_title(state: &ApiState, title: &str) -> String {
    media_for_title(state, title).to_string().replace('-', "")
}

/// 公开的作品 id（`w1_…`）：Series 条目与 `SeriesId` 用的就是它。
fn work_wire_id(media: MediaId) -> String {
    domain::TypedItemId::Work(media).to_wire_string()
}

/// 公开的分集 id（`u1_<media>_<season>_<episode>`）。
fn unit_wire_id(media: MediaId, season: u32, episode: u32) -> String {
    domain::TypedItemId::Unit {
        media,
        key: domain::UnitKey {
            season: season as i32,
            episode: episode as i32,
        },
    }
    .to_wire_string()
}

/// 按标题找这部剧的公开 `w1_…` id（`SeriesId` 期望的拼写）。
fn series_wire_id_for_title(state: &ApiState, title: &str) -> String {
    work_wire_id(media_for_title(state, title))
}

fn folder_id(root: &Path) -> String {
    format!(
        "folder_{}",
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_DNS,
            root.to_string_lossy().as_bytes()
        )
        .simple()
    )
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn get_json(app: &axum::Router, uri: &str) -> Value {
    let (status, body) = get(app, uri).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    body
}

fn items(body: &Value) -> &Vec<Value> {
    body["Items"].as_array().expect("Items array")
}

/// 页内 URL 的采纳对象：作品作用域给简介/类型/工作室，分集作用域给主图 `still_url`。
fn adopt_page_metadata(state: &ApiState, media: MediaId) {
    let fields = domain::MetadataFields {
        overview: domain::FieldValue::Value("adopted plot".into()),
        studios: domain::FieldValue::Value(vec!["Studio X".into()]),
        genres: domain::FieldValue::Value(vec![domain::MetadataGenre {
            source: "tmdb".into(),
            source_id: None,
            name: "Drama".into(),
            filter_key: "drama".into(),
        }]),
        still_url: domain::FieldValue::Value(
            "https://image.tmdb.org/t/p/w500/adopted-still.jpg".into(),
        ),
        ..Default::default()
    };
    let adoption = |scope: domain::EntityScope| domain::MetadataAdoption {
        media_id: media,
        scope,
        locale: "zh-CN".into(),
        source: "tmdb".into(),
        provider_entity_id: "100".into(),
        provider_work_id: "100".into(),
        fetched_at: 1,
        normalized: domain::NormalizedMetadata {
            fields: fields.clone(),
            field_origins: std::collections::BTreeMap::new(),
        },
        expected_revision: Some(0),
        preferences: domain::MetadataPreferences {
            language_priority: vec!["zh-CN".into()],
            cert_country_priority: Vec::new(),
        },
    };
    let store = state.store();
    let store = store.as_ref();
    store
        .adopt_metadata(&adoption(domain::EntityScope::Work), 1)
        .unwrap();
    store
        .adopt_metadata(
            &adoption(domain::EntityScope::Episode {
                season: 1,
                episode: 1,
            }),
            1,
        )
        .unwrap();
}

/// 采纳行在场时，页内主图只能来自已采纳事实（`still_url`）与本地文件约定；现场 NFO 被
/// 改坏、它引用的本地图片被删除，都不得改变同一个页的任何一个字段。
///
/// 出站网络由 fixture 的 [`NoFetch`] 全程拒绝，所以「离线 + 坏现场 NFO」这条差分里只剩
/// 现场 NFO 一个变量：`library::read_nfo` 读到的 `<thumb>` 一旦还参与渲染，页内
/// `ImageTags` 就会随 sidecar 消失，两个响应不再逐字段相等。
#[tokio::test]
async fn adopted_page_ignores_live_nfo_artwork() {
    let fixture = tv_fixture_seeded(1, 1, |show_dir| {
        std::fs::write(show_dir.join("reference-still.jpg"), b"jpeg").unwrap();
        std::fs::write(
            show_dir.join("01x01.nfo"),
            b"<episodedetails><title>Observed</title><thumb>reference-still.jpg</thumb></episodedetails>",
        )
        .unwrap();
    });
    let media = media_for_title(&fixture.state, "Show000");
    adopt_page_metadata(&fixture.state, media);
    let app = router(fixture.state.clone());
    let uri = format!(
        "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode&SortBy=SortName",
        fixture.library_id
    );
    let online = get_json(&app, &uri).await;
    assert_eq!(items(&online).len(), 1, "{online}");
    assert_eq!(
        items(&online)[0]["ImageTags"]["Primary"],
        "episode",
        "在线采纳页的主图必须成立：{online}"
    );

    let sidecar = fixture.roots[0].join("Show000").join("01x01.nfo");
    std::fs::write(&sidecar, b"<episodedetails><thumb>broken").unwrap();
    std::fs::remove_file(fixture.roots[0].join("Show000").join("reference-still.jpg")).unwrap();
    let offline = get_json(&app, &uri).await;
    assert_eq!(
        items(&offline)[0]["ImageTags"]["Primary"],
        "episode",
        "采纳行在场时页内主图不得来自现场 NFO 的 <thumb>：{offline}"
    );
    assert_eq!(
        offline, online,
        "现场 NFO 被改坏/删除不得改变采纳页的任何一个字段"
    );
}

/// 采纳页 + 现场 `<thumb>` 是远端形态：页内主图照旧成立，且现场 NFO 被改坏后响应不变。
#[tokio::test]
async fn adopted_page_keeps_its_primary_image_with_a_remote_nfo_thumb() {
    let fixture = tv_fixture_seeded(1, 1, |show_dir| {
        std::fs::write(
            show_dir.join("01x01.nfo"),
            b"<episodedetails><title>Observed</title>\
              <thumb>https://image.tmdb.org/t/p/w300/sidecar-still.jpg</thumb></episodedetails>",
        )
        .unwrap();
    });
    let media = media_for_title(&fixture.state, "Show000");
    adopt_page_metadata(&fixture.state, media);
    let app = router(fixture.state.clone());
    let uri = format!(
        "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode&SortBy=SortName",
        fixture.library_id
    );
    let online = get_json(&app, &uri).await;
    assert_eq!(
        items(&online)[0]["ImageTags"]["Primary"],
        "episode",
        "远端形态的现场 <thumb> 不得让采纳页丢主图：{online}"
    );

    std::fs::write(
        fixture.roots[0].join("Show000").join("01x01.nfo"),
        b"<episodedetails><thumb>broken",
    )
    .unwrap();
    let offline = get_json(&app, &uri).await;
    assert_eq!(
        offline, online,
        "页内主图只来自已落库字段，现场 NFO 被改坏不得改变页内响应"
    );
}

/// NFO-only 剧集（没有采纳行）：远端 `<thumb>` 经观察层落库后必须在页内生效，
/// 现场 NFO 事后被改坏也不得改变页内响应——页内不读现场 NFO。
#[tokio::test]
async fn nfo_only_remote_thumb_keeps_the_page_primary_image() {
    let fixture = tv_fixture_seeded(1, 1, |show_dir| {
        std::fs::write(
            show_dir.join("01x01.nfo"),
            b"<episodedetails><title>Observed</title>\
              <thumb>https://image.tmdb.org/t/p/w500/nfo-only-still.jpg</thumb></episodedetails>",
        )
        .unwrap();
    });
    let app = router(fixture.state.clone());
    let uri = format!(
        "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode&SortBy=SortName",
        fixture.library_id
    );
    let online = get_json(&app, &uri).await;
    assert_eq!(
        items(&online)[0]["ImageTags"]["Primary"],
        "episode",
        "只有 NFO <thumb> 有图的剧集必须在页内拿到主图：{online}"
    );

    std::fs::write(
        fixture.roots[0].join("Show000").join("01x01.nfo"),
        b"<episodedetails><thumb>broken",
    )
    .unwrap();
    let offline = get_json(&app, &uri).await;
    assert_eq!(
        offline, online,
        "页内主图只来自已落库字段，现场 NFO 被改坏不得改变页内响应"
    );
}

#[path = "jellyfin_paged_metadata/audit.rs"]
mod audit;
#[path = "jellyfin_paged_metadata/next_up.rs"]
mod next_up;
#[path = "jellyfin_paged_metadata/paged.rs"]
mod paged;
#[path = "jellyfin_paged_metadata/scope.rs"]
mod scope;

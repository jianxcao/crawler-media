//! `playback_clock` 用例共用的 fixture 与事实读取 helper：假时钟的 movie app、
//! 一条带位置的播放事件、以及播放次数 / 会话日志 / 历史观看时长的快照。

use super::*;
use axum::http::StatusCode;
use serde_json::json;

/// 假时钟 + movie 的 fixture：位置与墙钟断言必须落在确定值上。
pub(super) async fn movie_app_with_fake_clock() -> (
    tempfile::TempDir,
    axum::Router,
    Arc<FakePlaybackClock>,
    String,
) {
    let tmp = tempfile::tempdir().unwrap();
    let now = api::job_loop::unix_now();
    let clock = Arc::new(FakePlaybackClock(std::sync::atomic::AtomicI64::new(
        now * 1000,
    )));
    let app = app_with_clock(&tmp, clock.clone());
    let media_id = seed_movie(&app, &tmp).await;
    (tmp, app, clock, media_id)
}

/// 观影事实：播放次数、会话日志行数、历史条数与每条历史的观看时长。
pub(super) struct PlaybackFacts {
    pub(super) play_count: i64,
    pub(super) logs: i64,
    pub(super) entries: i64,
    pub(super) watched_ms: Vec<i64>,
}

pub(super) async fn playback_facts(
    app: &axum::Router,
    tmp: &tempfile::TempDir,
    media_id: &str,
) -> PlaybackFacts {
    let resume = get_data(
        app,
        &format!("/api/v1/playback/resume?media_item_id={media_id}"),
    )
    .await;
    let history = get_data(app, "/api/v1/playback/history").await;
    let entries = history["entries"].as_array().cloned().unwrap_or_default();
    let db = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
    let logs = db
        .query_row("SELECT COUNT(*) FROM playback_logs", [], |row| row.get(0))
        .unwrap();
    PlaybackFacts {
        play_count: resume["play_count"].as_i64().unwrap_or(-1),
        logs,
        entries: entries.len() as i64,
        watched_ms: entries
            .iter()
            .map(|entry| entry["watched_ms"].as_i64().unwrap_or(-1))
            .collect(),
    }
}

/// 发一条带位置的播放事件；`duration_ms` 只在 Stop 时给，用来判定「看完」。
pub(super) async fn post_event(
    app: &axum::Router,
    media_id: &str,
    device: &str,
    session: &str,
    event: &str,
    position_ms: i64,
    duration_ms: Option<i64>,
) -> StatusCode {
    let mut body = json!({ "media_item_id": media_id, "event": event, "device_id": device,
                          "play_session_id": session, "position_ms": position_ms });
    if let Some(duration_ms) = duration_ms {
        body["duration_ms"] = json!(duration_ms);
    }
    post_async(app, "/api/v1/playback/progress", body)
        .await
        .status()
}

/// 一次真实节奏的进度心跳：时钟与位置都按 20 秒步进，避免被判成 seek。
pub(super) async fn beat(
    app: &axum::Router,
    clock: &Arc<FakePlaybackClock>,
    media_id: &str,
    session: &str,
    step: u32,
) -> axum::response::Response {
    clock
        .0
        .fetch_add(20_000, std::sync::atomic::Ordering::SeqCst);
    post_async(
        app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "progress", "device_id": "web-1",
                "play_session_id": session,
                "position_ms": step * 20_000, "paused": false }),
    )
    .await
}

/// 直接写一条完结播放日志（`started_at`/`ended_at` 为 unix 秒），
/// 用来覆盖统计/历史的窗口边界。
pub(super) fn seed_playback_log(
    tmp: &tempfile::TempDir,
    media_id: &str,
    started_at: i64,
    ended_at: i64,
) {
    let conn = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
    conn.execute(
        "INSERT INTO playback_logs
             (id, user_id, media_id, device_id, started_at, ended_at, watched_ms,
              play_session_id, generation, file_id, file_revision, source_version, timeline_mode)
         VALUES (?1, ?2, ?3, 'seed-device', ?4, ?5, ?6,
                 ?7, 1, ?8, 1, 'seed-source', 'full_file')",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            "00000000-0000-0000-0000-000000000001",
            media_id,
            started_at,
            ended_at,
            30_000,
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        ],
    )
    .unwrap();
}

/// 8 天前的完结日志：真实时钟下落在 7 天窗口外，只进 previous 窗口。
pub(super) fn seed_old_playback_log(tmp: &tempfile::TempDir, media_id: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    seed_playback_log(tmp, media_id, now - 8 * 86_400, now - 8 * 86_400 + 60);
}

/// 假时钟 + 可读 store 的播放测试环境。
///
/// 假时钟停在 1 小时前：`activity` / `devices` / `end_session` / `revoke_device`
/// 任一回退到真实时钟，会话都会立刻被 300 秒超时判成不活跃。
pub(super) struct ClockEnv {
    pub(super) app: axum::Router,
    pub(super) clock: Arc<FakePlaybackClock>,
    pub(super) store: Arc<Mutex<api::Store>>,
    pub(super) user: domain::UserId,
    pub(super) media_id: String,
    pub(super) base_ms: i64,
}

impl ClockEnv {
    pub(super) async fn new(tmp: &tempfile::TempDir) -> Self {
        Self::with_base_ms(tmp, (api::job_loop::unix_now() - 3_600) * 1000).await
    }

    /// 以显式毫秒基准构造（marks/metrics 的秒边界用亚秒偏移）。
    pub(super) async fn with_base_ms(tmp: &tempfile::TempDir, base_ms: i64) -> Self {
        let clock = Arc::new(FakePlaybackClock(std::sync::atomic::AtomicI64::new(
            base_ms,
        )));
        let fetcher = Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        });
        let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
        let st = state(tmp.path(), fetcher, downloader);
        let app = router(st.clone().with_playback_clock(clock.clone()));
        let media_id = seed_movie(&app, tmp).await;
        let store = st.store();
        let user = {
            let guard = store.lock();
            guard
                .user_by_token("management-secret")
                .unwrap()
                .unwrap()
                .id
        };
        Self {
            app,
            clock,
            store,
            user,
            media_id,
            base_ms,
        }
    }

    /// 在 `web-1` 上 start + 一次心跳，返回心跳后的假时钟毫秒值。
    pub(super) async fn start_web_session(&self) -> i64 {
        let session = open_play_session(&self.app, &self.media_id, "web-1").await;
        let response = post_async(
            &self.app,
            "/api/v1/playback/progress",
            json!({ "media_item_id": self.media_id, "event": "start", "device_id": "web-1", "play_session_id": session, "position_ms": 0 }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            beat(&self.app, &self.clock, &self.media_id, &session, 1)
                .await
                .status(),
            StatusCode::OK
        );
        self.base_ms + 20_000
    }
}

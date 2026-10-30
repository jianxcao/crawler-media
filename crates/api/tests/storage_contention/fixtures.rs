//! 测试夹具：建库 / 建 Library 与 Media / 造 `LibraryFile` / 发真实 `publish_files` 与真实心跳，
//! 以及用一条原始连接写入「无关用户」事实。这里不含度量口径与断言。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::Router;
use axum::http::StatusCode;
use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    Media, MediaId, MediaKind, QualitySource, UnitKey, UserId,
};
use parking_lot::Mutex;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use store::StoreError;
use store::library_write::{FilePublication as StorePublication, PublishReport, WriteScope};
use tower::ServiceExt;

use crate::harness::open_db;

pub(crate) const TOKEN: &str = "management-secret";
/// 片段时长；只要心跳带上 `duration_ms`，就不会走 probe 入队那条额外写路径。
pub(crate) const SERIES_DURATION_MS: i64 = 3_600_000;
pub(crate) const SUMMARY_SQL: &str = "SELECT metadata_revision || '|' || item_revision || '|' || file_count || '|' \
     || available_file_count || '|' || missing_file_count || '|' || unknown_file_count || '|' \
     || distinct_episode_count || '|' || distinct_season_count || '|' || COALESCE(preferred_file_id, '-') \
     FROM library_items WHERE library_id = ?1 AND media_id = ?2";

pub(crate) struct Fixture {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) state: ApiState,
    pub(crate) app: Router,
    pub(crate) series_library: LibraryId,
    pub(crate) series_root: PathBuf,
    pub(crate) series_root_id: String,
    pub(crate) series: Media,
    /// 「不拥有该单元」的另一个 Library（另一部片、另一条根）。
    pub(crate) unowned_library: String,
}

/// 建一个 Library + 一条根 + 一个 Media，返回 (library_id, root_id, root_path, media)。
pub(crate) fn seed_library(
    state: &ApiState,
    dir: &Path,
    kind: MediaKind,
    name: &str,
    folder: &str,
) -> (LibraryId, String, PathBuf, Media) {
    let binding = state.store();
    let store = binding.as_ref();
    let root = dir.join(folder);
    let library = store
        .create_library(kind, name, &[root.to_str().unwrap()], "everyone", true, &[])
        .unwrap();
    let (root_id, root_path) = root_of(store, &library.id);
    let media = media(kind, name);
    store.insert_media(&media).unwrap();
    (library.id.parse().unwrap(), root_id, root_path, media)
}

/// 固定播放时钟（task-98）：心跳的 `now_ms` 会经 `user_summary` 的
/// `last_played_at = max(watch_activity_at)`（`crates/store/src/playback/user_summary/mod.rs:510-515`）
/// 落到 `user_library_items` 那一行，而该行只在内容变化时才被重写（同文件 `:346-350`）。
/// 因此「写作用域是否随无关数据增长而扩大」的比较必须消掉墙钟变量：否则两个测量窗口之间
/// 隔着的秒数本身就会多写一行，把时间差误读成写放大。
pub(crate) const FIXED_NOW_MS: i64 = 1_792_595_400_000;

#[derive(Clone, Copy)]
pub(crate) struct FixedPlaybackClock;

impl api::playback_events::PlaybackClock for FixedPlaybackClock {
    fn now_ms(&self) -> i64 {
        FIXED_NOW_MS
    }
}

pub(crate) fn fixture(title: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::common::state(
        dir.path(),
        Arc::new(crate::common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage"))),
    )
    .with_playback_clock(Arc::new(FixedPlaybackClock));
    let app = router(state.clone());
    let (series_library, series_root_id, series_root, series) =
        seed_library(&state, dir.path(), MediaKind::Tv, title, "series");
    let (unowned_library, unowned_root_id, unowned_root, movie) = seed_library(
        &state,
        dir.path(),
        MediaKind::Movie,
        "Unowned Movie Library",
        "unowned",
    );
    {
        let binding = state.store();
        let store = binding.as_ref();
        let template = file_template(series_library, &series_root_id, series.id, MediaKind::Tv);
        let episode_one = episode_file(&template, &series_root, 1, 1);
        publish(
            store,
            &series_library,
            &[episode_one],
            &[UnitKey::new(1, 1)],
            1_000,
        )
        .unwrap();

        let template = file_template(
            unowned_library,
            &unowned_root_id,
            movie.id,
            MediaKind::Movie,
        );
        let movie_file = LibraryFile {
            file_id: LedgerId::new(),
            path: unowned_root.join("unowned.mkv"),
            ..template
        };
        publish(
            store,
            &unowned_library,
            &[movie_file],
            &[UnitKey::WHOLE],
            1_500,
        )
        .unwrap();
    }
    Fixture {
        dir,
        state,
        app,
        series_library,
        series_root,
        series_root_id,
        series,
        unowned_library: unowned_library.to_string(),
    }
}

pub(crate) fn media(kind: MediaKind, title: &str) -> Media {
    Media {
        id: MediaId::new(),
        kind,
        title: title.to_string(),
        year: Some(2020),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

pub(crate) fn root_of(store: &Store, library_id: &str) -> (String, PathBuf) {
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id.as_str() == library_id)
        .expect("Library 必须有一条根");
    (root.id, root.path)
}

pub(crate) fn file_template(
    library_id: LibraryId,
    root_id: &str,
    media_id: MediaId,
    kind: MediaKind,
) -> LibraryFile {
    LibraryFile {
        file_id: LedgerId::new(),
        library_id,
        root_id: root_id.to_string(),
        media_id,
        kind,
        path: PathBuf::new(),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(1 << 30),
        stat_known: true,
        created_ns: Some(1_000),
        modified_ns: Some(2_000),
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: Some("1080p".into()),
        codec: Some("h264".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: Some(SERIES_DURATION_MS),
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    }
}

pub(crate) fn episode_file(
    template: &LibraryFile,
    root: &Path,
    season: i32,
    episode: i32,
) -> LibraryFile {
    LibraryFile {
        file_id: LedgerId::new(),
        path: root.join(format!("S{season:02}E{episode:04}.mkv")),
        ..template.clone()
    }
}

/// 走真实的 `publish_files`（每批一个事务），不绕过生产写入路径。
pub(crate) fn publish(
    store: &Store,
    library_id: &LibraryId,
    files: &[LibraryFile],
    keys: &[UnitKey],
    now_ms: i64,
) -> Result<PublishReport, StoreError> {
    assert_eq!(files.len(), keys.len(), "文件与单元必须一一对应");
    let publications: Vec<StorePublication<'_>> = files
        .iter()
        .zip(keys.iter())
        .map(|(file, key)| StorePublication {
            file,
            units: std::slice::from_ref(key),
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        })
        .collect();
    store.library_write(|connection| {
        let config_revision = connection.library_config_revision(&library_id.to_string())?;
        connection.publish_files(
            &WriteScope {
                library_id: *library_id,
                config_revision,
            },
            &publications,
            now_ms,
        )
    })
}

pub(crate) fn heartbeat_body(
    series: MediaId,
    position_ms: i64,
    device: &str,
    session: &str,
) -> Value {
    json!({
        "device_id": device,
        "play_session_id": session,
        "event_sequence": crate::common::next_playback_sequence(),
        "event": "progress",
        "media_item_id": series.to_string(),
        "season_number": 1,
        "episode_number": 1,
        "position_ms": position_ms,
        "duration_ms": SERIES_DURATION_MS,
    })
}

/// The benchmark client obtains its real binding before measuring heartbeat work.
pub(crate) async fn open_play_session(app: &Router, series: MediaId, device: &str) -> String {
    let response = app.clone().oneshot(crate::common::request(
        "POST", "/api/v1/playback/sessions", Some(TOKEN),
        json!({"media_item_id":series.to_string(),"season_number":1,"episode_number":1,"device_id":device}),
    )).await.unwrap();
    let body = crate::common::json_data(response).await;
    body["play_session_id"].as_str().unwrap().to_string()
}

/// 打真实路由：`/api/v1/playback/heartbeat` 不存在，真实路径是 `/playback/progress`。
pub(crate) async fn progress(app: &Router, body: Value) -> Value {
    let response = app
        .clone()
        .oneshot(crate::common::request(
            "POST",
            "/api/v1/playback/progress",
            Some(TOKEN),
            body,
        ))
        .await
        .unwrap();
    let status = response.status();
    let payload = crate::common::json_body(response).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "心跳必须成功（POST /api/v1/playback/progress）: {payload}"
    );
    assert_eq!(payload["ok"], true, "心跳响应信封: {payload}");
    payload
}

pub(crate) async fn beat(
    app: &Router,
    series: MediaId,
    position_ms: i64,
    device: &str,
    session: &str,
) {
    progress(app, heartbeat_body(series, position_ms, device, session)).await;
}

/// 首个事件的 `start` 形状：生产客户端先开播放会话再发 `start`；`progress` 是后续心跳的形状。
/// 预热心跳必须用这一形状，否则「无会话的首次 progress」不是真实生产序列。
pub(crate) fn start_body(series: MediaId, position_ms: i64, device: &str, session: &str) -> Value {
    let mut body = heartbeat_body(series, position_ms, device, session);
    body["event"] = Value::String("start".to_string());
    body
}

pub(crate) async fn beat_start(
    app: &Router,
    series: MediaId,
    position_ms: i64,
    device: &str,
    session: &str,
) {
    progress(app, start_body(series, position_ms, device, session)).await;
}

pub(crate) fn revision(state: &ApiState, library_id: &str) -> i64 {
    let binding = state.store();
    let store = binding.as_ref();
    store.library_config_revision(library_id).unwrap()
}

/// 用一条原始连接写入一大批**无关用户**的播放事实。
///
/// 列名与 CHECK 约束都按 `schema/v1/library.sql` 写实：`playback_units` 没有 `summary_revision`
/// 列；`resume_*` 全 NULL 时 `position_ms` 必须为 0。错误一律不吞（`.unwrap()`），
/// 否则「表没变」可能只是 INSERT 全失败；行数由调用方断言。
pub(crate) fn grow_unrelated_users(
    db: &Path,
    media_id: MediaId,
    library_id: &LibraryId,
    users: usize,
) -> (usize, usize) {
    let conn = open_db(db);
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut units = conn
        .prepare(
            "INSERT INTO playback_units
               (user_id, media_id, season, episode, played, has_played_assertion,
                favorite, play_count, watch_activity_at, position_ms)
             VALUES (?1, ?2, 1, ?3, 0, 0, 0, 0, ?4, 0)",
        )
        .unwrap();
    let mut items = conn
        .prepare(
            "INSERT INTO user_library_items
               (user_id, library_id, media_id, seen, played, favorite,
                owned_episode_count, completed_episode_count, summary_revision)
             VALUES (?1, ?2, ?3, 0, 0, 0, 0, 0, 0)",
        )
        .unwrap();
    for index in 0..users {
        let user = UserId::new().to_string();
        units
            .execute(rusqlite::params![
                user,
                media_id.to_string(),
                (index % 3_600) as i64 + 1,
                10_000 + index as i64
            ])
            .unwrap();
        items
            .execute(rusqlite::params![
                user,
                library_id.to_string(),
                media_id.to_string()
            ])
            .unwrap();
    }
    conn.execute_batch("COMMIT").unwrap();
    (users, users)
}

pub(crate) fn file_rows(db: &Path, file_id: &LedgerId) -> i64 {
    open_db(db)
        .query_row(
            "SELECT COUNT(*) FROM library_files WHERE file_id IN (?1, ?2)",
            rusqlite::params![file_id.to_string(), file_id.to_string().replace('-', "")],
            |row| row.get(0),
        )
        .unwrap()
}

pub(crate) fn summary_tuple(db: &Path, library_id: &str, media_id: &str) -> Option<String> {
    open_db(db)
        .query_row(
            SUMMARY_SQL,
            rusqlite::params![library_id, media_id],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

pub(crate) fn scan_status(db: &Path, id: &str) -> String {
    open_db(db)
        .query_row(
            "SELECT status FROM scan_operations WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}

//! Plan Task 17 Step 1：在新存储模型上守住整条
//! 「空库扫描 → Subscribe/Scrape → 浏览/播放 → 重启 → 清 cache → 离线 → remove/reimport」
//! 链路，并断言旧 shape（旧 Filter DSL、明文口令、旧 file-ID 逻辑路由、非规范 ID）
//! 被明确拒绝。
//!
//! 只使用 `tests/management/common.rs` 已有的测试设施：临时 data dir、
//! Fixture Catalog/Downloader、内存 Downloader。不触网、不起 Chromium、不用 sleep 轮询。

#[path = "management/common.rs"]
mod common;

#[path = "storage_end_to_end/contracts.rs"]
mod contracts;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    Media, MediaId, MediaKind, QualitySource, UnitKey, UserId,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use store::library_write::{FilePublication as StorePublication, WriteScope};
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "management-secret";
const MEMBER_LOGIN: &str = "e2e-member";
const MEMBER_PASSWORD: &str = "member-pass";
const EPISODE_NAME: &str = "Identity.Show.2025.S01E03.strm";
const SHOW_TITLE: &str = "Identity Show";
const TMDB_ID: &str = "207890";
const SEASON: i32 = 1;
const EPISODE: i32 = 3;
const LEGACY_DSL_ATOMS: &str = "100:resolution=2160p|50:source=bluray";
const LEGACY_PLAIN_LOGIN: &str = "legacy-plain";
const LEGACY_PLAIN_PASSWORD: &str = "plain-secret-1";

/// 旧 shape 行只能直接落库：`filters` / `users` 表都在 `data/app.db`。
fn write_legacy_row(temp: &TempDir, sql: &str, params: &[&dyn rusqlite::ToSql]) {
    let db = temp.path().join("data").join("app.db");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute(sql, params).unwrap();
}

fn insert_legacy_filter_row(temp: &TempDir, id: &str) {
    write_legacy_row(
        temp,
        "INSERT INTO filters (id, name, atoms_json) VALUES (?1, ?2, ?3)",
        rusqlite::params![id, "legacy-dsl", LEGACY_DSL_ATOMS],
    );
}

fn insert_plaintext_user_row(temp: &TempDir, id: &str) {
    write_legacy_row(
        temp,
        "INSERT INTO users (id, login, password, enabled, role) VALUES (?1, ?2, ?3, 1, 'member')",
        rusqlite::params![id, LEGACY_PLAIN_LOGIN, LEGACY_PLAIN_PASSWORD],
    );
}

/// 离线目录：任何远端查询都失败，证明链路不依赖真实 TMDB。
struct OfflineCatalog;

impl api::catalog::Catalog for OfflineCatalog {
    fn search_movie(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Err("offline fixture".into())
    }

    fn search_tv(&self, _query: &str) -> Result<Vec<media::CatalogHit>, String> {
        Err("offline fixture".into())
    }

    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Err("offline fixture".into())
    }

    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Err("offline fixture".into())
    }

    fn details(&self, _kind: MediaKind, _tmdb_id: &str) -> Result<Option<Media>, String> {
        Err("offline fixture".into())
    }
}

/// 观看快照：条目身份 + 收藏标记，全部来自公开 DTO。
#[derive(Debug, PartialEq)]
struct Adopted {
    media_id: String,
    favorite: bool,
}

struct Fixture {
    temp: TempDir,
    state: api::ApiState,
    app: Router,
    library_id: String,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = boot(temp.path()).with_catalog(Arc::new(OfflineCatalog));
        // 预置的默认 TV 库受保护、不可删除；端到端链路要真的走一次删除，
        // 所以这里用 store 建一个独立的 TV 库（不经 HTTP，避免自动扫描抢租约）。
        let root = temp.path().join("e2e-library");
        std::fs::create_dir_all(&root).unwrap();
        let library_id = {
            let handle = state.store();
            let store = handle.as_ref();
            store
                .create_library(
                    MediaKind::Tv,
                    "E2E Tv Library",
                    &[root.to_str().unwrap()],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap()
                .id
        };
        let app = api::router(state.clone());
        Self {
            temp,
            state,
            app,
            library_id,
            root,
        }
    }

    /// 模拟重启：同一 data dir 上重建 ApiState 与 router。
    fn reboot(&self) -> (api::ApiState, Router) {
        let state = boot(self.temp.path()).with_catalog(Arc::new(OfflineCatalog));
        (state.clone(), api::router(state))
    }

    /// 写入 `.strm` + `tvshow.nfo` + `poster.jpg`，并把该文件发布进新模型。
    fn seed_episode(&self, file_id: LedgerId) -> MediaId {
        let path = self.root.join(EPISODE_NAME);
        std::fs::write(&path, b"identity-stand-in").unwrap();
        std::fs::write(
            self.root.join("tvshow.nfo"),
            format!("<tvshow><title>{SHOW_TITLE}</title><tmdbid>{TMDB_ID}</tmdbid></tvshow>"),
        )
        .unwrap();
        std::fs::write(self.root.join("poster.jpg"), b"jpeg-stand-in").unwrap();

        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: SHOW_TITLE.into(),
            year: Some(2025),
            original_title: None,
            aliases: vec![],
            tmdb_id: Some(TMDB_ID.into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let handle = self.state.store();
        let store = handle.as_ref();
        store.insert_media(&media).unwrap();
        let root = store
            .list_library_roots()
            .unwrap()
            .into_iter()
            .find(|root| root.library_id.as_str() == self.library_id.as_str())
            .expect("TV 库必须有根目录");
        let file = LibraryFile {
            file_id,
            library_id: self.library_id.parse::<LibraryId>().unwrap(),
            root_id: root.id.clone(),
            media_id: media.id,
            kind: MediaKind::Tv,
            path: path.clone(),
            version: FileVersion {
                revision: 1,
                source_version: "v1".into(),
            },
            presence: FilePresence::Present,
            publication: FilePublication::Published,
            size_bytes: Some(13),
            stat_known: true,
            created_ns: Some(0),
            modified_ns: Some(0),
            source_digest: None,
            source_path: None,
            transfer_mode: None,
            resolution: Some("1080p".into()),
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: Some(100_000),
            tracks_version: 1,
            poster_path: None,
            backdrop_path: None,
            still_path: None,
        };
        let publication = StorePublication {
            file: &file,
            units: &[UnitKey::new(SEASON, EPISODE)],
            expected_version: None,
            nfo_observation: None,
            probe_required: false,
        };
        let scope = WriteScope {
            library_id: file.library_id,
            config_revision: 0,
        };
        store.publish_files(&scope, &[publication], 1_000).unwrap();
        media.id
    }
}

fn boot(root: &Path) -> api::ApiState {
    common::state(
        root,
        Arc::new(common::Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(downloader::MemoryDownloader::new(root.join("stage"))),
    )
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(common::request(method, uri, token, body))
        .await
        .unwrap()
}

async fn login(app: &Router, username: &str, password: &str) -> String {
    let response = send(
        app,
        "POST",
        "/api/v1/auth/login",
        None,
        json!({ "username": username, "password": password }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "成员必须可以登录");
    common::json_data(response).await["token"]
        .as_str()
        .expect("登录响应必须带 token")
        .to_string()
}

async fn browse(app: &Router, library_id: &str) -> Vec<Value> {
    let response = send(
        app,
        "GET",
        &format!("/api/v1/libraries/{library_id}/items"),
        Some(TOKEN),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "目录浏览必须成功");
    common::json_data(response)
        .await
        .as_array()
        .expect("目录浏览必须返回数组")
        .clone()
}

async fn snapshot(app: &Router, library_id: &str, token: &str) -> Adopted {
    let items = browse(app, library_id).await;
    assert_eq!(items.len(), 1, "期望恰好一个条目，实际 {items:?}");
    let media_id = items[0]["media_item_id"]
        .as_str()
        .expect("卡片必须带 media_item_id")
        .to_string();
    let response = send(
        app,
        "GET",
        &format!(
            "/api/v1/playback/marks?media_item_id={media_id}&season_number={SEASON}&episode_number={EPISODE}"
        ),
        Some(token),
        Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "观看状态必须可读");
    let marks = common::json_data(response).await;
    Adopted {
        media_id,
        favorite: marks["is_favorite"].as_bool().unwrap_or(false),
    }
}

async fn watch(app: &Router, media_id: &str, token: &str) {
    let issued = send(
        app,
        "POST",
        "/api/v1/playback/sessions",
        Some(token),
        json!({
            "media_item_id":media_id,"device_id":"e2e-device",
            "season_number":SEASON,"episode_number":EPISODE,
        }),
    )
    .await;
    assert_eq!(
        issued.status(),
        StatusCode::OK,
        "Playback client must obtain a binding"
    );
    let issued = common::json_data(issued).await;
    let session = issued["play_session_id"].as_str().unwrap();
    let progress = send(
        app,
        "POST",
        "/api/v1/playback/progress",
        Some(token),
        json!({
            "media_item_id": media_id,
            "device_id": "e2e-device",
            "play_session_id": session,
            "event_sequence":common::next_playback_sequence(),
            "event":"start",
            "season_number": SEASON,
            "episode_number": EPISODE,
            "position_ms": 1_000,
            "duration_ms": 100_000,
            "rate": 1.0,
        }),
    )
    .await;
    assert_eq!(progress.status(), StatusCode::OK, "播放心跳必须被接受");
    let marks = send(
        app,
        "POST",
        "/api/v1/playback/marks",
        Some(token),
        json!({
            "media_item_id": media_id,
            "season_number": SEASON,
            "episode_number": EPISODE,
            "played": true,
            "favorite": true,
        }),
    )
    .await;
    assert_eq!(marks.status(), StatusCode::OK, "标记必须被接受");
    let body = common::json_data(marks).await;
    assert_eq!(body["is_favorite"], true, "收藏必须写回: {body}");
}

async fn subscribe_tv(app: &Router, library_id: &str) {
    let mut payload = common::subscribe_payload("search");
    payload["library_id"] = json!(library_id);
    payload["media"] = json!({ "kind": "tv", "title": SHOW_TITLE });
    payload["coverage"] = json!({
        "kind": "tv",
        "season": SEASON,
        "episode_from": EPISODE,
        "episode_to": EPISODE,
    });
    let response = send(app, "POST", "/api/v1/subscriptions", Some(TOKEN), payload).await;
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "Subscribe 必须创建成功"
    );
}

async fn scrape(app: &Router, library_id: &str) {
    let response = send(
        app,
        "POST",
        &format!("/api/v1/libraries/{library_id}/metadata/refresh"),
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "离线 Scrape 必须降级而不是失败"
    );
}

async fn remove_library(app: &Router, library_id: &str) {
    let response = send(
        app,
        "DELETE",
        &format!("/api/v1/libraries/{library_id}"),
        Some(TOKEN),
        Value::Null,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "删除媒体库必须是异步操作"
    );
    let operation_id = common::json_data(response).await["operation_id"]
        .as_str()
        .expect("删除操作必须带 operation_id")
        .to_string();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let polled = send(
                app,
                "GET",
                &format!("/api/v1/library-removals/{operation_id}"),
                Some(TOKEN),
                Value::Null,
            )
            .await;
            assert_eq!(polled.status(), StatusCode::OK, "删除操作必须可查询");
            let body = common::json_data(polled).await;
            match body["state"].as_str() {
                Some("succeeded") => break,
                Some("failed") => panic!("删除媒体库失败: {body}"),
                _ => tokio::task::yield_now().await,
            }
        }
    })
    .await
    .expect("删除媒体库必须在 10 秒内收敛");
}

/// 用同一目录重新建库；建库会自动触发一次扫描，所以这里不重复 `POST /scan`。
async fn import_library(app: &Router, root: &Path) -> String {
    let response = send(
        app,
        "POST",
        "/api/v1/libraries",
        Some(TOKEN),
        json!({
            "kind": "tv",
            "name": "E2E Reimport",
            "root_paths": [root.display().to_string()],
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "重新建库必须成功");
    common::json_data(response).await["id"]
        .as_str()
        .expect("建库响应必须带 id")
        .to_string()
}

/// 建库自带的首次扫描是后台任务，这里用有界让渡轮询直到条目出现（不用 sleep）。
async fn await_reimport(app: &Router, library_id: &str) -> Vec<Value> {
    for _ in 0..5_000 {
        let items = browse(app, library_id).await;
        if !items.is_empty() {
            return items;
        }
        tokio::task::yield_now().await;
    }
    panic!("重新导入后扫描没有收敛出条目");
}

#[tokio::test]
async fn clean_start_restart_clear_cache_remove_purges_orphan_media_and_reimport_starts_fresh() {
    let fixture = Fixture::new();
    let app = fixture.app.clone();
    common::create_member(&app, MEMBER_LOGIN).await;
    let mut token = login(&app, MEMBER_LOGIN, MEMBER_PASSWORD).await;

    // 空库：显式扫描完成且没有任何条目。
    common::scan_and_wait(&app, &fixture.library_id).await;
    assert!(
        browse(&app, &fixture.library_id).await.is_empty(),
        "空库扫描后不应有任何条目"
    );

    // Subscribe（typed Filter atoms）与 Scrape（离线降级）。
    subscribe_tv(&app, &fixture.library_id).await;
    scrape(&app, &fixture.library_id).await;

    // 浏览 / 播放：文件走新模型 publish_files，旧台账不再参与。
    let file_id = LedgerId::new();
    fixture.seed_episode(file_id);
    let adopted = snapshot(&app, &fixture.library_id, &token).await;
    let original_media_id = adopted.media_id.clone();
    watch(&app, &original_media_id, &token).await;
    let adopted_before_cache_clear = snapshot(&app, &fixture.library_id, &token).await;
    assert!(
        adopted_before_cache_clear.favorite,
        "收藏必须写进 playback_units 并被读回"
    );

    // 重启：同一 data dir 上重建 ApiState，成员重新登录。
    let (state, app) = fixture.reboot();
    token = login(&app, MEMBER_LOGIN, MEMBER_PASSWORD).await;

    // 清 cache：清空搜索快照与指纹缓存。注意这与按键的
    // `DELETE /catalog/cache?cache_key=` 语义不同（那条是单键失效），
    // 这里按 store 级清洗，用于证明 cache 不是身份的来源。
    {
        let handle = state.store();
        let store = handle.as_ref();
        store.clear_search_snapshots().unwrap();
        store
            .delete_fingerprint_cache(&file_id.to_string())
            .unwrap();
    }
    let after_cache_clear = snapshot(&app, &fixture.library_id, &token).await;

    // 离线：remove 后用同一目录、同一份 .nfo 重新导入。
    remove_library(&app, &fixture.library_id).await;
    let reimported = import_library(&app, &fixture.root).await;
    await_reimport(&app, &reimported).await;
    let after_reimport = snapshot(&app, &reimported, &token).await;
    let old_file_id_item_response = send(
        &app,
        "GET",
        &format!("/api/v1/libraries/{reimported}/items/{file_id}"),
        Some(TOKEN),
        Value::Null,
    )
    .await;

    // cache 前后快照必须一致（cache 不是身份来源）。删除 Library 时失去最后一个
    // 文件的 Media 连同其收藏/播放进度一起清除，所以重新导入得到全新 Media，
    // 旧收藏不会回来；旧 file-ID 逻辑路由必须被拒。
    assert_eq!(after_cache_clear, adopted_before_cache_clear);
    assert_ne!(
        after_reimport.media_id, original_media_id,
        "孤儿 Media 必须随 Library 删除被清除，重新导入是新身份"
    );
    assert!(!after_reimport.favorite, "被清除 Media 的收藏不能复活");
    let purged = send(
        &app,
        "GET",
        &format!("/api/v1/libraries/{reimported}/items/{original_media_id}"),
        Some(TOKEN),
        Value::Null,
    )
    .await;
    assert_eq!(
        purged.status(),
        StatusCode::NOT_FOUND,
        "已清除的 Media 不可再打开"
    );
    assert_eq!(old_file_id_item_response.status(), StatusCode::NOT_FOUND);

    // 反向对照：真正的 media item id 仍然 200，说明上面的 404 来自
    // 「物理 file id 不是 item id」，而不是路由整体失效。
    let detail = send(
        &app,
        "GET",
        &format!(
            "/api/v1/libraries/{reimported}/items/{}",
            after_reimport.media_id
        ),
        Some(TOKEN),
        Value::Null,
    )
    .await;
    assert_eq!(detail.status(), StatusCode::OK, "媒体条目本身必须可以打开");
}

/// 旧 Filter DSL 字符串是旧 shape：读路径必须 400，而不是尽力解码。
#[tokio::test]
async fn legacy_filter_dsl_is_rejected() {
    let fixture = Fixture::new();
    let app = fixture.app.clone();
    let legacy_filter_id = LedgerId::new().to_string();
    insert_legacy_filter_row(&fixture.temp, &legacy_filter_id);

    let legacy_filter = send(
        &app,
        "GET",
        &format!("/api/v1/rule-sets/{legacy_filter_id}"),
        Some(TOKEN),
        Value::Null,
    )
    .await;
    assert_eq!(
        legacy_filter.status(),
        StatusCode::BAD_REQUEST,
        "旧 Filter DSL 字符串必须被判为不可解析的旧 shape"
    );
    let error = common::json_error(legacy_filter).await;
    assert_eq!(
        error["code"], "rule_set.invalid",
        "错误码必须指向 Filter 解析"
    );

    // 正向对照：typed atoms 必须正常往返，证明上面的 400 来自旧 shape 本身。
    let created = send(
        &app,
        "POST",
        "/api/v1/rule-sets",
        Some(TOKEN),
        json!({
            "name": "typed-atoms",
            "atoms": [
                { "kind": "resolution", "value": "2160p", "priority": 100 },
                { "kind": "title_match", "value": "Identity", "priority": 80 },
            ],
        }),
    )
    .await;
    assert_eq!(
        created.status(),
        StatusCode::CREATED,
        "typed atoms 必须可创建"
    );
    let typed_id = common::json_data(created).await["id"]
        .as_str()
        .expect("Filter 必须带 id")
        .to_string();
    let round_trip = send(
        &app,
        "GET",
        &format!("/api/v1/rule-sets/{typed_id}"),
        Some(TOKEN),
        Value::Null,
    )
    .await;
    assert_eq!(
        round_trip.status(),
        StatusCode::OK,
        "typed atoms 必须可读回"
    );
    let round_trip = common::json_data(round_trip).await;
    assert_eq!(
        round_trip["atoms"].as_array().map(Vec::len),
        Some(2),
        "typed atoms 必须原样往返: {round_trip}"
    );
}

/// 明文口令行是旧 shape：不得静默升级并放行。
#[tokio::test]
async fn plaintext_password_is_rejected() {
    let fixture = Fixture::new();
    let app = fixture.app.clone();
    insert_plaintext_user_row(&fixture.temp, &UserId::new().to_string());

    let plaintext_login = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        None,
        json!({ "username": LEGACY_PLAIN_LOGIN, "password": LEGACY_PLAIN_PASSWORD }),
    )
    .await;
    assert_eq!(
        plaintext_login.status(),
        StatusCode::UNAUTHORIZED,
        "明文口令行不得被静默升级并放行"
    );

    // 正向对照：Argon2id 口令必须能登录并重复登录。
    let (_, member_token) = common::create_member(&app, MEMBER_LOGIN).await;
    assert!(
        !member_token.is_empty(),
        "Argon2id 口令必须可以登录并签发 token"
    );
    assert!(
        !login(&app, MEMBER_LOGIN, MEMBER_PASSWORD).await.is_empty(),
        "Argon2id 口令必须可以重复登录"
    );
}

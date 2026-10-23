//! Playback behavior driven by the injected `PlaybackClock`: activity /
//! devices / end-session / revoke, session issue time, stats + history
//! windows, sub-second heartbeat persistence, and the seconds/milliseconds
//! boundary of the web marks and metrics writes.
//!
//! HTTP helpers shared with the sibling `playback` module live there
//! (`post_async`, `get_data`, `seed_movie`); the fake-clock fixtures live here.

use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;
use super::playback::{get_data, open_play_session, post_async, seed_movie};

mod fixtures;
use fixtures::*;

pub(super) struct FakePlaybackClock(pub(super) std::sync::atomic::AtomicI64);

impl api::playback_events::PlaybackClock for FakePlaybackClock {
    fn now_ms(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

pub(super) fn app_with_clock(
    tmp: &tempfile::TempDir,
    clock: Arc<FakePlaybackClock>,
) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader).with_playback_clock(clock))
}

#[tokio::test]
async fn progress_lifecycle_writes_session_log_and_unit() {
    let tmp = tempfile::tempdir().unwrap();
    let now = api::job_loop::unix_now();
    let clock = Arc::new(FakePlaybackClock(std::sync::atomic::AtomicI64::new(
        now * 1000,
    )));
    let app = app_with_clock(&tmp, clock.clone());
    let media_id = seed_movie(&app, &tmp).await;
    let session = open_play_session(&app, &media_id, "web-1").await;

    let start = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "start", "device_id": "web-1", "play_session_id": session, "position_ms": 0 }),
    )
    .await;
    assert_eq!(start.status(), StatusCode::OK);
    let body = json_body(start).await;
    assert_eq!(body["data"]["played"], false);
    assert_eq!(body["data"]["position_ms"], 0);

    // 真实节奏心跳（每步 20 秒，<= 30s，不会被判成 seek）。
    for step in 1..=6 {
        let response = beat(&app, &clock, &media_id, &session, step).await;
        assert_eq!(response.status(), StatusCode::OK);
        if step == 6 {
            assert_eq!(json_body(response).await["data"]["position_ms"], 120_000);
        }
    }

    // Active session shows in activity.
    let activity = get_data(&app, "/api/v1/playback/activity").await;
    let sessions = activity["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["device_id"], "web-1");
    assert_eq!(
        sessions[0]["revocable"], false,
        "web player has no credentials"
    );
    assert_eq!(sessions[0]["media"]["title"], "The Matrix");

    for step in 7..=14 {
        assert_eq!(
            beat(&app, &clock, &media_id, &session, step).await.status(),
            StatusCode::OK
        );
    }

    // Stop closes the session into a play log.
    clock
        .0
        .fetch_add(20_000, std::sync::atomic::Ordering::SeqCst);
    let stop = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "stop", "device_id": "web-1", "play_session_id": session, "position_ms": 300_000 }),
    )
    .await;
    assert_eq!(stop.status(), StatusCode::OK);

    let activity = get_data(&app, "/api/v1/playback/activity").await;
    assert!(activity["sessions"].as_array().unwrap().is_empty());

    let history = get_data(&app, "/api/v1/playback/history").await;
    let entries = history["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["media"]["title"], "The Matrix");
    assert_eq!(entries[0]["watched_ms"], 120_000 + 180_000);
    assert_eq!(entries[0]["completed"], false);

    // Resume reads the unit.
    let resume = get_data(
        &app,
        &format!("/api/v1/playback/resume?media_item_id={media_id}"),
    )
    .await;
    assert_eq!(resume["position_ms"], 300_000);
}

#[tokio::test]
async fn injected_clock_drives_activity_devices_and_end_session() {
    let tmp = tempfile::tempdir().unwrap();
    let env = ClockEnv::new(&tmp).await;
    let beat_ms = env.start_web_session().await;

    let activity = get_data(&env.app, "/api/v1/playback/activity").await;
    let sessions = activity["sessions"].as_array().unwrap();
    assert_eq!(
        sessions.len(),
        1,
        "注入时钟下 1 小时前的会话仍应活跃: {activity}"
    );
    assert_eq!(sessions[0]["last_report_at"], beat_ms.to_string());

    let devices = get_data(&env.app, "/api/v1/playback/devices").await;
    let device = devices
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["device_id"] == "web-1")
        .expect("活跃设备应出现在 devices");
    assert_eq!(device["active_sessions"], 1);
    assert_eq!(device["last_seen_at"], beat_ms.to_string());

    // end_session 写回的 last_report_at 必须来自注入时钟。
    let ended = post_async(
        &env.app,
        "/api/v1/playback/activity/sessions/web-1/end",
        Value::Null,
    )
    .await;
    assert_eq!(ended.status(), StatusCode::OK);
    let session = env
        .store
        .lock()
        .get_session(env.user, "web-1")
        .unwrap()
        .expect("会话行仍在（admin_ended），只是不再活跃");
    assert!(session.admin_ended);
    assert_eq!(
        session.last_report_at,
        beat_ms / 1000,
        "end_session 必须用注入时钟"
    );
    let activity = get_data(&env.app, "/api/v1/playback/activity").await;
    assert!(activity["sessions"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn injected_clock_drives_revoke_device() {
    let tmp = tempfile::tempdir().unwrap();
    let env = ClockEnv::new(&tmp).await;
    // 注销需要已绑定的会话凭据（web 会话本身不可注销）。
    {
        let guard = env.store.lock();
        guard.set_user_token(env.user, "tv-token").unwrap();
        guard
            .bind_playback_credential(env.user, "web-1", "tv-token")
            .unwrap();
    }
    let beat_ms = env.start_web_session().await;

    // 注销用注入时钟关闭会话：落库日志的 ended_at 决定 devices 的 last_seen_at。
    let revoke = env
        .app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/playback/devices/web-1?user_id={}", env.user),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(revoke.status(), StatusCode::OK);

    let devices = get_data(&env.app, "/api/v1/playback/devices").await;
    let revoked = devices
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["device_id"] == "web-1")
        .expect("已注销设备应出现在 devices");
    assert_eq!(
        revoked["last_seen_at"],
        beat_ms.to_string(),
        "注销日志的 ended_at 必须用注入时钟"
    );

    let blocked = post_async(
        &env.app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": env.media_id, "event": "progress", "device_id": "web-1", "position_ms": 10 }),
    )
    .await;
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn stats_aggregate_and_policy_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let now = api::job_loop::unix_now();
    let clock = Arc::new(FakePlaybackClock(std::sync::atomic::AtomicI64::new(
        now * 1000,
    )));
    let app = app_with_clock(&tmp, clock.clone());
    let media_id = seed_movie(&app, &tmp).await;
    let session = open_play_session(&app, &media_id, "web-1").await;
    let start = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "start", "device_id": "web-1", "play_session_id": session, "position_ms": 0 }),
    )
    .await;
    assert_eq!(start.status(), StatusCode::OK);
    for step in 1..=2 {
        assert_eq!(
            beat(&app, &clock, &media_id, &session, step).await.status(),
            StatusCode::OK
        );
    }
    clock
        .0
        .fetch_add(20_000, std::sync::atomic::Ordering::SeqCst);
    let stop = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "stop", "device_id": "web-1", "play_session_id": session, "position_ms": 60_000 }),
    )
    .await;
    assert_eq!(stop.status(), StatusCode::OK);

    // 8 天前的完结日志只进 previous 窗口，用于覆盖 retention 边界。
    seed_old_playback_log(&tmp, &media_id);

    let stats = get_data(&app, "/api/v1/playback/stats/watch?days=7").await;
    assert_eq!(stats["current"]["plays"], 1);
    assert!(stats["current"]["watched_ms"].as_i64().unwrap() >= 60_000);
    assert_eq!(stats["by_day"].as_array().unwrap().len(), 7);
    assert_eq!(stats["by_hour"].as_array().unwrap().len(), 7);
    assert_eq!(stats["by_hour"][0].as_array().unwrap().len(), 24);
    assert!(
        stats["by_day"]
            .as_array()
            .unwrap()
            .iter()
            .any(|day| day["members"] == 1)
    );
    assert_eq!(
        stats["by_member"][0]["member_id"],
        "00000000-0000-0000-0000-000000000001"
    );
    assert!(stats["top_titles"][0]["watched_ms"].as_i64().unwrap() >= 60_000);
    assert_eq!(stats["top_titles"][0]["members"], 1);
    assert_eq!(stats["previous"]["active_members"], 1);
    assert_eq!(stats["previous_by_day"].as_array().unwrap().len(), 7);
    assert!(
        stats["previous_by_day"]
            .as_array()
            .unwrap()
            .iter()
            .any(|day| day["plays"] == 1)
    );

    let policy = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/playback/policy",
            Some("management-secret"),
            json!({ "trickplay_enabled": true }),
        ))
        .await
        .unwrap();
    assert_eq!(policy.status(), StatusCode::OK);
    let policy_get = get_data(&app, "/api/v1/playback/policy").await;
    assert_eq!(policy_get["trickplay_enabled"], true);
    assert_eq!(policy_get["hardware_available"], false);
}

/// `POST /playback/sessions` 的签发时间必须来自注入时钟（秒）。
///
/// 假时钟比真实时钟慢 1 小时：任一回退到 `unix_now()` 都会让落库的
/// `issued_at` 与注入时钟相差 3600 秒。
#[tokio::test]
async fn session_issue_uses_injected_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let base_ms = (api::job_loop::unix_now() - 3_600) * 1000;
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
    let media_id = seed_movie(&app, &tmp).await;

    let response = post_async(
        &app,
        "/api/v1/playback/sessions",
        json!({ "media_item_id": media_id, "device_id": "clock-device" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    let session_id = body["data"]["play_session_id"]
        .as_str()
        .expect("会话必须签发 play_session_id")
        .to_string();

    let store = st.store();
    let user = {
        let guard = store.lock();
        guard
            .user_by_token("management-secret")
            .unwrap()
            .unwrap()
            .id
    };
    let binding = store
        .lock()
        .play_session_binding(user, "clock-device", &session_id)
        .unwrap()
        .expect("签发的会话必须落库");
    assert_eq!(
        binding.issued_at,
        base_ms / 1000,
        "会话签发时间必须来自注入时钟（秒）"
    );
}

/// 统计/历史的窗口起点必须来自注入时钟（秒）。
///
/// 假时钟比真实时钟快 40 天：真实时钟下两条日志都落在 7 天窗口内（未来时间
/// 无上界），注入时钟下只有靠近假时钟的那条在内。断言 1 而非 2 即区分两者。
#[tokio::test]
async fn stats_and_history_windows_follow_injected_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let real_now = api::job_loop::unix_now();
    let base_ms = (real_now + 40 * 86_400) * 1000;
    let clock = Arc::new(FakePlaybackClock(std::sync::atomic::AtomicI64::new(
        base_ms,
    )));
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader).with_playback_clock(clock.clone()));
    let media_id = seed_movie(&app, &tmp).await;

    let base_secs = base_ms / 1000;
    seed_playback_log(&tmp, &media_id, real_now, real_now + 60);
    seed_playback_log(&tmp, &media_id, base_secs - 60, base_secs);

    let stats = get_data(&app, "/api/v1/playback/stats/watch?days=7").await;
    assert_eq!(
        stats["current"]["plays"], 1,
        "统计当前窗口必须以注入时钟为起点: {stats}"
    );

    let history = get_data(&app, "/api/v1/playback/history?days=7").await;
    assert_eq!(
        history["entries"].as_array().unwrap().len(),
        1,
        "历史 days 过滤必须以注入时钟为起点: {history}"
    );
}

/// 注入的 `PlaybackClock` 必须按毫秒落库：心跳路径不得把 `last_report_at_ms`
/// 量化回整秒（B3 复核：`now * 1000` 会丢掉亚秒精度）。
#[tokio::test]
async fn heartbeat_persists_sub_second_report_time() {
    let tmp = tempfile::tempdir().unwrap();
    let base_ms = 1_700_000_000_000i64;
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
    let media_id = seed_movie(&app, &tmp).await;
    let session = open_play_session(&app, &media_id, "web-ms").await;

    let start = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "start", "device_id": "web-ms", "play_session_id": session, "position_ms": 0 }),
    )
    .await;
    assert_eq!(start.status(), StatusCode::OK);

    // 只推进 500ms：若实现按秒量化，这个差值会丢失。
    clock.0.fetch_add(500, std::sync::atomic::Ordering::SeqCst);
    let beat = post_async(
        &app,
        "/api/v1/playback/progress",
        json!({ "media_item_id": media_id, "event": "progress", "device_id": "web-ms", "play_session_id": session, "position_ms": 0, "paused": false }),
    )
    .await;
    assert_eq!(beat.status(), StatusCode::OK);

    let store = st.store();
    let user = {
        let guard = store.lock();
        guard
            .user_by_token("management-secret")
            .unwrap()
            .unwrap()
            .id
    };
    let session = store.lock().get_session(user, "web-ms").unwrap().unwrap();
    assert_eq!(
        session.last_report_at_ms,
        base_ms + 500,
        "心跳必须保留注入时钟的亚秒精度"
    );
}

/// `POST /playback/marks` 落库的 `updated_at` 必须是注入时钟的秒值。
///
/// 假时钟 = 真实时钟 - 1 小时 + 500ms：回退到 `unix_now()` 会差 3600 秒，
/// 直接把毫秒写进秒列会差 ~1.7e12，两者都会被下面的断言抓住。
#[tokio::test]
async fn web_marks_persist_injected_clock_units() {
    let tmp = tempfile::tempdir().unwrap();
    let base_ms = (api::job_loop::unix_now() - 3_600) * 1000 + 500;
    let env = ClockEnv::with_base_ms(&tmp, base_ms).await;

    let marked = post_async(
        &env.app,
        "/api/v1/playback/marks",
        json!({ "media_item_id": env.media_id, "played": true, "favorite": true }),
    )
    .await;
    assert_eq!(marked.status(), StatusCode::OK);

    let media_id: domain::MediaId = env.media_id.parse().unwrap();
    let unit = env
        .store
        .lock()
        .unit_state(env.user, media_id, -1, -1)
        .unwrap()
        .expect("marks 必须写入 playback_units");
    assert_eq!(
        unit.played_assertion_at_ms,
        Some(base_ms),
        "played 断言时间是注入时钟的毫秒值"
    );
    assert_eq!(unit.favorite_at, base_ms / 1000, "favorite_at 是秒");
    assert_eq!(unit.updated_at, 0, "标记不是观看活动，不推进 activity");
}

/// `POST /playback/metrics` 落库的 `recorded_at` 必须是注入时钟的秒值。
///
/// 与 marks 同一时钟契约：两个 web 路由都必须走注入时钟，否则同一个持久化
/// 事实会由两个时钟产生。
#[tokio::test]
async fn web_metrics_persist_injected_clock_seconds() {
    let tmp = tempfile::tempdir().unwrap();
    let base_ms = (api::job_loop::unix_now() - 3_600) * 1000 + 500;
    let env = ClockEnv::with_base_ms(&tmp, base_ms).await;

    let recorded = post_async(
        &env.app,
        "/api/v1/playback/metrics",
        json!({ "media_item_id": env.media_id, "tier": 0, "engine": "web", "watched_ms": 1_000 }),
    )
    .await;
    assert_eq!(recorded.status(), StatusCode::OK);

    let db = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
    let recorded_at: i64 = db
        .query_row(
            "SELECT recorded_at FROM playback_metrics WHERE media_id = ?1",
            rusqlite::params![env.media_id],
            |row| row.get(0),
        )
        .expect("metrics 必须落库");
    assert_eq!(
        recorded_at,
        base_ms / 1000,
        "web metrics 必须按注入时钟的秒值落库"
    );
}
#[tokio::test]
async fn repeated_start_increments_play_count_once() {
    let (tmp, app, clock, media_id) = movie_app_with_fake_clock().await;
    let session = open_play_session(&app, &media_id, "web-1").await;

    // Start 幂等：同一会话重复 Start（含播放中途再来一次）后仍然只算一次播放。
    assert_eq!(
        post_event(&app, &media_id, "web-1", &session, "start", 0, None).await,
        StatusCode::OK
    );
    assert_eq!(
        post_event(&app, &media_id, "web-1", &session, "start", 60_000, None).await,
        StatusCode::OK
    );
    for step in 1..=3 {
        assert_eq!(
            beat(&app, &clock, &media_id, &session, step).await.status(),
            StatusCode::OK
        );
    }
    clock
        .0
        .fetch_add(20_000, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        post_event(&app, &media_id, "web-1", &session, "start", 60_000, None).await,
        StatusCode::OK
    );
    for step in 4..=7 {
        assert_eq!(
            beat(&app, &clock, &media_id, &session, step).await.status(),
            StatusCode::OK
        );
    }
    clock
        .0
        .fetch_add(20_000, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        post_event(
            &app,
            &media_id,
            "web-1",
            &session,
            "stop",
            140_000,
            Some(150_000)
        )
        .await,
        StatusCode::OK
    );

    let facts = playback_facts(&app, &tmp, &media_id).await;
    assert_eq!(facts.play_count, 1, "重复 Start 只算一次播放");
    assert_eq!(
        (facts.logs, facts.entries),
        (1, 1),
        "一条会话日志、一条历史"
    );
    assert_eq!(
        facts.watched_ms,
        vec![120_000],
        "重复 Start 不把观看时间翻倍"
    );

    // 重复 Stop 同样不重复计数、不追加观看时间。
    assert_eq!(
        post_event(
            &app,
            &media_id,
            "web-1",
            &session,
            "stop",
            140_000,
            Some(150_000)
        )
        .await,
        StatusCode::OK
    );
    let repeated = playback_facts(&app, &tmp, &media_id).await;
    assert_eq!(
        (repeated.play_count, repeated.logs, repeated.entries),
        (1, 1, 1),
        "重复 Stop 不重复计数"
    );
    assert_eq!(
        repeated.watched_ms,
        vec![120_000],
        "重复 Stop 不追加观看时间"
    );
}

#[tokio::test]
async fn progress_heartbeats_do_not_increment_play_count() {
    let (tmp, app, clock, media_id) = movie_app_with_fake_clock().await;
    let session = open_play_session(&app, &media_id, "web-1").await;
    assert_eq!(
        post_event(&app, &media_id, "web-1", &session, "start", 0, None).await,
        StatusCode::OK
    );

    let mut last = None;
    for step in 1..=7 {
        let response = beat(&app, &clock, &media_id, &session, step).await;
        assert_eq!(response.status(), StatusCode::OK);
        last = Some(json_body(response).await);
    }
    assert_eq!(
        last.unwrap()["data"]["position_ms"],
        140_000,
        "进度事件确实写进了会话位置（否则下面的 0 会是空断言）"
    );

    let facts = playback_facts(&app, &tmp, &media_id).await;
    assert_eq!(facts.play_count, 0, "一串进度事件不加 play_count");
    assert_eq!(
        (facts.logs, facts.entries),
        (0, 0),
        "进度事件不产生会话日志与历史"
    );

    // 只有真实的 Stop 才 +1。
    assert_eq!(
        post_event(
            &app,
            &media_id,
            "web-1",
            &session,
            "stop",
            140_000,
            Some(150_000)
        )
        .await,
        StatusCode::OK
    );
    let after_stop = playback_facts(&app, &tmp, &media_id).await;
    assert_eq!(after_stop.play_count, 1, "真实 Stop 才 +1");
    assert_eq!((after_stop.logs, after_stop.entries), (1, 1));
}

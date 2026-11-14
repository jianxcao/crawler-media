#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use axum::http::StatusCode;
use domain::{LibraryFile, UserId};
use serde_json::{Value, json};
use support::library_presence::PresenceFixture;

struct FakeClock(AtomicI64);

impl api::playback_events::PlaybackClock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// 注入时钟的 fixture：`PresenceFixture` 默认用真实墙钟，位置跳变的断言必须落在
/// 确定值上，所以把 state 换成带假时钟的克隆（store 仍是同一个 `Arc`）。
fn clocked_fixture(base_ms: i64) -> (PresenceFixture, Arc<FakeClock>) {
    let mut fix = PresenceFixture::new();
    let clock = Arc::new(FakeClock(AtomicI64::new(base_ms)));
    fix.state = fix.state.clone().with_playback_clock(clock.clone());
    (fix, clock)
}

async fn progress(
    fix: &PresenceFixture,
    row: &LibraryFile,
    device: &str,
    session: Option<&str>,
    event: &str,
    position_ms: i64,
) -> (StatusCode, Value) {
    let mut body = json!({
        "event_sequence": support::session::next_sequence(),
        "device_id": device,
        "media_item_id": row.media_id.to_string(),
        "file_id": row.file_id.to_string(),
        "event": event,
        "position_ms": position_ms,
    });
    if let Some(session) = session {
        body["play_session_id"] = json!(session);
    }
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        body,
    )
    .await
}

/// 会话行的 `(watched_ms, position_ms)`。位置一并断言，是为了证明跳变事件确实落库
/// ——否则「观看时间没变」还可能只是事件被整体忽略，断言会变成假绿。
fn session_state(fix: &PresenceFixture, user_id: UserId, device: &str) -> (i64, i64) {
    let store_arc = fix.state.store();
    let guard = store_arc.as_ref();
    let row = guard
        .get_session(user_id, device)
        .unwrap()
        .expect("session row must exist");
    (row.watched_ms, row.position_ms)
}

fn admin_user(fix: &PresenceFixture) -> UserId {
    let store_arc = fix.state.store();
    let guard = store_arc.as_ref();
    guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
}

#[tokio::test]
async fn watched_time_accumulates_correctly_and_ignores_seeks() {
    let (fix, clock) = clocked_fixture(1_000_000);
    let (_, row) = fix.insert_media_and_ledger("clock_movie.mkv");
    let user_id = admin_user(&fix);

    let session_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.as_ref();
        support::session::issue_session(guard, user_id, "dev_clock", row.file_id, 1000)
    };

    // 1. Start at t = 1000s, position 0：只开会话，不进账。
    let (status, _) = progress(&fix, &row, "dev_clock", Some(&session_id), "start", 0).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        session_state(&fix, user_id, "dev_clock"),
        (0, 0),
        "Start 只开会话，观看时间仍为 0"
    );

    // 2. 先做出非零基线：真实流逝 10 秒、位置前进 10_000ms，进账 10_000ms。
    clock.0.fetch_add(10_000, Ordering::SeqCst);
    let (status, _) = progress(
        &fix,
        &row,
        "dev_clock",
        Some(&session_id),
        "progress",
        10_000,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        session_state(&fix, user_id, "dev_clock"),
        (10_000, 10_000),
        "正常播放：10 秒真实流逝进账 10_000ms"
    );

    // 3. 真实流逝 10 秒、位置跳到 610_000ms（向前 seek +600_000）：事件照常落库
    //    （位置推进到 610_000），但 600 秒跳变距离一分不进账。
    clock.0.fetch_add(10_000, Ordering::SeqCst);
    let (status, _) = progress(
        &fix,
        &row,
        "dev_clock",
        Some(&session_id),
        "progress",
        610_000,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        session_state(&fix, user_id, "dev_clock"),
        (10_000, 610_000),
        "向前 seek 落库新位置，但不把跳变距离计入观看时间"
    );

    // 4. seek 之后恢复正常播放（真实 +10 秒、位置 +10_000）：观看时间继续累积。
    clock.0.fetch_add(10_000, Ordering::SeqCst);
    let (status, _) = progress(
        &fix,
        &row,
        "dev_clock",
        Some(&session_id),
        "progress",
        620_000,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        session_state(&fix, user_id, "dev_clock"),
        (20_000, 620_000),
        "seek 之后的正常播放照常进账"
    );

    // 5. 再真实流逝 10 秒、位置回退 300_000ms（向后 seek）：跳变距离同样不进账。
    clock.0.fetch_add(10_000, Ordering::SeqCst);
    let (status, _) = progress(
        &fix,
        &row,
        "dev_clock",
        Some(&session_id),
        "progress",
        320_000,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        session_state(&fix, user_id, "dev_clock"),
        (20_000, 320_000),
        "向后 seek 不得把跳变距离计入观看时间"
    );
}

#[tokio::test]
async fn missing_session_events_do_not_create_watch_time_or_resume_facts() {
    let (fix, clock) = clocked_fixture(1_000_000);
    let (_, row) = fix.insert_media_and_ledger("unbound_seek.mkv");
    let user = admin_user(&fix);
    for (event, position) in [
        ("start", 0),
        ("progress", 10_000),
        ("progress", 600_000),
        ("stop", 600_000),
    ] {
        clock.0.fetch_add(10_000, Ordering::SeqCst);
        let (status, _) = progress(&fix, &row, "unbound", None, event, position).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let shared = fix.state.store();
    let store = shared.as_ref();
    assert!(store.get_session(user, "unbound").unwrap().is_none());
    assert!(
        store
            .unit_state(user, row.media_id, -1, -1)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .list_logs(10, None, None, None, i64::MAX / 2, None)
            .unwrap()
            .is_empty()
    );
}

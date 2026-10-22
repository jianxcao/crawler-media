#[path = "support/mod.rs"]
mod support;

use axum::http::StatusCode;
use serde_json::json;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn late_old_stop_does_not_close_new_session() {
    let fix = PresenceFixture::new();
    let (_, row_a) = fix.insert_media_and_ledger("movie_a.mkv");
    let (_, row_b) = fix.insert_media_and_ledger("movie_b.mkv");

    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    // 1. Start session A
    let session_id_a = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, "dev1", row_a.file_id, 1000)
    };

    // Start A with play_session_id_a
    let (status, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": row_a.media_id.to_string(),
                "file_id": row_a.file_id.to_string(),
                "event": "start",
                "position_ms": 100,
                "play_session_id": session_id_a,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // 2. Start session B on same device
    let session_id_b = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, "dev1", row_b.file_id, 2000)
    };
    let (status, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": row_b.media_id.to_string(),
                "file_id": row_b.file_id.to_string(),
                "event": "start",
                "position_ms": 200,
                "play_session_id": session_id_b,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // Verify active session is B
    let store_arc = fix.state.store();
    let current = store_arc.lock().get_session(user_id, "dev1").unwrap().unwrap();
    assert_eq!(current.ledger_id.as_deref(), Some(&row_b.file_id.to_string()[..]));
    assert_eq!(current.play_session_id.as_deref(), Some(session_id_b.as_str()));

    // 3. Late STOP from session A arrives!
    let (status, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": row_a.media_id.to_string(),
                "file_id": row_a.file_id.to_string(),
                "event": "stop",
                "position_ms": 150,
                "play_session_id": session_id_a,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // Session B must NOT be closed!
    let current_after = store_arc.lock().get_session(user_id, "dev1").unwrap();
    assert!(current_after.is_some(), "Session B must still be active");
    assert_eq!(
        current_after.unwrap().play_session_id.as_deref(),
        Some(session_id_b.as_str())
    );
}

#[tokio::test]
async fn late_old_progress_does_not_rewind_position() {
    let fix = PresenceFixture::new();
    let (_, row) = fix.insert_media_and_ledger("same_movie.mkv");
    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    // Session 1 on same file
    let id_1 = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, "dev1", row.file_id, 1000)
    };
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.file_id.to_string(),
            "event": "start",
            "position_ms": 5000,
            "play_session_id": id_1,
        }),
    ).await;

    // Session 2 on same file (newer generation)
    let id_2 = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, "dev1", row.file_id, 2000)
    };
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.file_id.to_string(),
            "event": "start",
            "position_ms": 8000,
            "play_session_id": id_2,
        }),
    ).await;

    // Late progress from session 1 with position 2000 arrives
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.file_id.to_string(),
            "event": "progress",
            "position_ms": 2000,
            "play_session_id": id_1,
        }),
    ).await;

    // Active session position must remain 8000, not rewound to 2000
    let store_arc = fix.state.store();
    let cur = store_arc.lock().get_session(user_id, "dev1").unwrap().unwrap();
    assert_eq!(cur.position_ms, 8000);
}

#[tokio::test]
async fn issued_session_cannot_write_replacement_source() {
    let fix = PresenceFixture::new();
    let (_media_a, row_a) = fix.insert_media_and_ledger("file_a.mkv");
    let (media_b, row_b) = fix.insert_media_and_ledger("file_b.mkv");

    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    let session_id_a = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, "dev1", row_a.file_id, 1000)
    };

    // Client sends heartbeat for media_b with session_id_a (issued for row_a / media_a)
    // Event should be rejected/ignored and not write to unit or session
    let (status, _body) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": media_b.to_string(),
                "file_id": row_b.file_id.to_string(),
                "event": "progress",
                "position_ms": 10000,
                "play_session_id": session_id_a,
            }),
        )
        .await;
    // Protocol contract: success response (IgnoredStale), but no state was updated
    assert_eq!(status, StatusCode::OK);

    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    let unit_b = guard.unit_state(user_id, media_b, -1, -1).unwrap();
    assert!(unit_b.is_none() || unit_b.unwrap().position_ms != 10000);
}

#[tokio::test]
async fn late_other_episode_does_not_close_active_session() {
    let fix = PresenceFixture::new();
    let (_, row_a) = fix.insert_media_and_ledger("ep1.mkv");
    let (_, row_b) = fix.insert_media_and_ledger("ep2.mkv");

    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    // Start session on B (active)
    let session_id_b = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, "dev1", row_b.file_id, 2000)
    };
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row_b.media_id.to_string(),
            "file_id": row_b.file_id.to_string(),
            "event": "start",
            "position_ms": 200,
            "play_session_id": session_id_b,
        }),
    )
    .await;

    // Late stop without ID for A arrives
    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row_a.media_id.to_string(),
            "file_id": row_a.file_id.to_string(),
            "event": "stop",
            "position_ms": 150,
        }),
    )
    .await;

    // Active session B must not be closed!
    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    let active = guard.get_session(user_id, "dev1").unwrap().unwrap();
    assert_eq!(active.play_session_id.as_deref(), Some(session_id_b.as_str()));
}

#[tokio::test]
async fn duplicate_stop_only_records_one_log() {
    let fix = PresenceFixture::new();
    let (_, row) = fix.insert_media_and_ledger("single.mkv");

    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };

    let session_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, "dev1", row.file_id, 1000)
    };

    fix.request_with_body(
        "POST",
        "/api/v1/playback/progress",
        Some(&fix.admin_token),
        json!({
            "device_id": "dev1",
            "media_item_id": row.media_id.to_string(),
            "file_id": row.file_id.to_string(),
            "event": "start",
            "position_ms": 1000,
            "play_session_id": session_id,
        }),
    )
    .await;

    // First stop
    let (s1, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": row.media_id.to_string(),
                "file_id": row.file_id.to_string(),
                "event": "stop",
                "position_ms": 5000,
                "play_session_id": session_id,
            }),
        )
        .await;
    assert_eq!(s1, StatusCode::OK);

    // Duplicate stop
    let (s2, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": row.media_id.to_string(),
                "file_id": row.file_id.to_string(),
                "event": "stop",
                "position_ms": 5000,
                "play_session_id": session_id,
            }),
        )
        .await;
    assert_eq!(s2, StatusCode::OK);

    let store_arc = fix.state.store();
    let guard = store_arc.lock();
    let logs = guard
        .list_logs(10, None, None, None, i64::MAX / 2, None)
        .unwrap();
    assert_eq!(logs.len(), 1, "Duplicate stop should only generate 1 log entry");
}

/// RED(task-88 必做#2/#3):绑定会话的物理源已经换版(revision 前进)时,
/// 迟到的 progress/stop 必须整体 IgnoredStale —— unit 事实与会话行都不许变。
///
/// 判别点:纯分类器看不到物理源事实,会把这两个事件判成 Update/Stop;
/// 于是只走 legacy 写路径的旧实现会改写 position,并在 stop 上删掉会话行。
#[tokio::test]
async fn stale_source_event_after_revision_bump_writes_nothing() {
    let fix = PresenceFixture::new();
    let (media_id, row) = fix.insert_media_and_ledger("stale_source.mkv");
    let store_arc = fix.state.store();
    let user_id = {
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };
    let device = "dev_stale_src";
    let sid = {
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, device, row.file_id, 1000)
    };

    let (status, body) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": device,
                "media_item_id": media_id.to_string(),
                "file_id": row.file_id.to_string(),
                "play_session_id": sid,
                "event": "start",
                "position_ms": 1000,
                "duration_ms": 600_000,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "start body={body}");

    let unit_before = {
        let guard = store_arc.lock();
        guard.unit_state(user_id, media_id, -1, -1).unwrap().unwrap()
    };
    assert_eq!(unit_before.position_ms, 1000);
    let session_before = {
        let guard = store_arc.lock();
        guard.get_session(user_id, device).unwrap().unwrap()
    };

    // 物理源换版:事件携带的 file_revision 从此过期。
    // raw 连接与 review_remediation_playback 的 seeding 手法一致。
    let db = rusqlite::Connection::open(fix.dir.path().join("data/library.db")).unwrap();
    let bumped = db
        .execute(
            "UPDATE library_files SET file_revision = file_revision + 1 WHERE file_id = ?1",
            rusqlite::params![row.file_id.to_string()],
        )
        .unwrap();
    assert_eq!(bumped, 1, "fixture 必须恰好换版一行");
    drop(db);

    for event in ["timeupdate", "stop"] {
        let (status, body) = fix
            .request_with_body(
                "POST",
                "/api/v1/playback/progress",
                Some(&fix.admin_token),
                json!({
                    "device_id": device,
                    "media_item_id": media_id.to_string(),
                    "file_id": row.file_id.to_string(),
                    "play_session_id": sid,
                    "event": event,
                    "position_ms": 300_000,
                    "duration_ms": 600_000,
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "event={event} body={body}");

        let unit_after = {
            let guard = store_arc.lock();
            guard.unit_state(user_id, media_id, -1, -1).unwrap().unwrap()
        };
        assert_eq!(
            (unit_after.position_ms, unit_after.played),
            (unit_before.position_ms, unit_before.played),
            "{event} 携带的物理源已换版,必须 IgnoredStale 零写入"
        );

        let session_after = {
            let guard = store_arc.lock();
            guard.get_session(user_id, device).unwrap()
        };
        let session_after = session_after.expect("源换版不得删除会话行(IgnoredStale 零写入)");
        assert_eq!(
            (session_after.position_ms, session_after.watched_ms),
            (session_before.position_ms, session_before.watched_ms),
            "{event} 携带的物理源已换版,会话行不得变化"
        );
    }
}

/// §16.3 ④(HTTP 调用点):`playback_session_ids` 必须走完 pending → active → closed,
/// 并且 closed 之后的迟到事件 **不复活** 绑定(零新增 live 行)。
///
/// 判别点:HTTP 回包 200 只说明协议成功,绑定态必须直接读 `playback_session_ids`
/// 才能证明 canonical 真的推进了状态机。
#[tokio::test]
async fn http_events_move_the_issued_session_id_through_activate_and_close() {
    use store::playback::PlaySessionIdState;

    let fix = PresenceFixture::new();
    let (media_id, row) = fix.insert_media_and_ledger("session_states.mkv");
    let store_arc = fix.state.store();
    let user_id = {
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };
    let device = "dev_session_states";
    let sid = {
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, device, row.file_id, 1_000)
    };
    let state = |sid: &str| {
        store_arc
            .lock()
            .play_session_binding(user_id, device, sid)
            .unwrap()
            .map(|binding| binding.state)
    };
    assert_eq!(state(&sid), Some(PlaySessionIdState::Pending), "签发态必须是 pending");

    let fixture = &fix;
    let post = |event: &str, position_ms: i64| {
        let body = json!({
            "device_id": device,
            "media_item_id": media_id.to_string(),
            "file_id": row.file_id.to_string(),
            "play_session_id": sid,
            "event": event,
            "position_ms": position_ms,
            "duration_ms": 600_000,
        });
        let fixture = fixture;
        async move {
            fixture
                .request_with_body("POST", "/api/v1/playback/progress", Some(&fixture.admin_token), body)
                .await
        }
    };

    // activate:start 必须把绑定推到 active 并建 live 行。
    let (start_status, start_body) = post("start", 1_000).await;
    assert_eq!(start_status, StatusCode::OK, "start body={start_body}");
    assert_eq!(state(&sid), Some(PlaySessionIdState::Active), "start 后绑定必须是 active");
    assert!(store_arc.lock().get_session(user_id, device).unwrap().is_some(), "active 绑定必须有 live 行");

    // close:stop 必须把绑定推到 closed 并归档 live 行。
    let (stop_status, stop_body) = post("stop", 5_000).await;
    assert_eq!(stop_status, StatusCode::OK, "stop body={stop_body}");
    assert_eq!(state(&sid), Some(PlaySessionIdState::Closed), "stop 后绑定必须是 closed");
    assert!(store_arc.lock().get_session(user_id, device).unwrap().is_none(), "closed 绑定不得留 live 行");

    // Ignore 不复活:同一个 closed 绑定再来 progress,协议 200 但状态机不动。
    let (late_status, late_body) = post("progress", 9_000).await;
    assert_eq!(late_status, StatusCode::OK, "late body={late_body}");
    assert_eq!(state(&sid), Some(PlaySessionIdState::Closed), "Ignore 不得复活 closed 绑定");
    assert!(
        store_arc.lock().get_session(user_id, device).unwrap().is_none(),
        "Ignore 不得新建 live 行"
    );
}

/// ⑭ G-1(覆盖缺口):「绑定单元 ≠ 事件目标单元」必须判 `Ignore`,判别点就是
/// `playback_events.rs:88-91` 的 `Some(unit) => b.unit == unit`(全仓零覆盖)。
///
/// 为什么钉在公开判定入口而不是 HTTP:`crates/store/src/playback/event.rs:289-290`
/// 的 `binding_matches` 用 `binding.season == unit.season && binding.episode == unit.episode`
/// 独立拦下同一事件,两层判定逐字节等价。实测:单独变异任一层,HTTP 用例都仍然全绿;
/// 只有两层同时变异才翻红 ⇒ HTTP 层给不了 `:88-91` 任何检测力。
#[test]
fn binding_unit_mismatch_is_ignored_at_decision_seam() {
    let ledger = domain::LedgerId::new();
    let binding = store::PlaySessionBinding {
        ledger_id: ledger,
        issued_at: 1_000,
        generation: 1,
        state: store::PlaySessionIdState::Pending,
        media_id: domain::MediaId::new(),
        unit: domain::UnitKey::new(1, 2),
        file_id: None,
        file_revision: None,
        source_version: None,
        library_id: None,
        config_revision: 0,
    };
    let identity = api::playback_events::PlaybackEventIdentity {
        play_session_id: Some("sess-unit-mismatch".to_string()),
        event_sequence: None,
    };
    let classify = |target: domain::UnitKey| {
        api::playback_events::classify_event_with_target(
            None,
            Some(&binding),
            &identity,
            ledger,
            Some(target),
            api::playback_events::PlaybackEventKind::Progress,
        )
    };
    assert_eq!(
        classify(domain::UnitKey::new(1, 1)),
        api::playback_events::EventDecision::Ignore,
        "绑定单元 1x2 上的 1x1 事件必须判 Ignore"
    );
    assert_eq!(
        classify(domain::UnitKey::new(1, 2)),
        api::playback_events::EventDecision::Start,
        "正控制:同单元事件必须判 Start(pending 会话被激活)"
    );
}

/// ⑭ G-2(覆盖缺口):同会话内 `event_sequence` 回退(2 → 1)必须命中
/// `sequence_regressed` 真支 ⇒ `Ignore` ⇒ 零写入。
///
/// 判别点:`playback_events.rs:106-107` 的 `seq <= prev_seq`(全仓零覆盖;既有 10 处
/// 测试构造全是 `last_event_sequence: None`)。只用「序号缺失 ⇒ false」的实现会
/// 把回退的 progress 当成正常前进,改写位置。
#[tokio::test]
async fn http_event_sequence_regression_is_ignored_and_writes_nothing() {
    let fix = PresenceFixture::new();
    let (media_id, row) = fix.insert_media_and_ledger("seq_regression.mkv");
    let store_arc = fix.state.store();
    let user_id = {
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };
    let device = "dev_seq_regression";
    let sid = {
        let guard = store_arc.lock();
        support::session::issue_session(&guard, user_id, device, row.file_id, 1_000)
    };
    let post = |event: &str, position_ms: i64, event_sequence: i64| {
        let body = json!({
            "device_id": device,
            "media_item_id": media_id.to_string(),
            "file_id": row.file_id.to_string(),
            "play_session_id": sid,
            "event": event,
            "position_ms": position_ms,
            "duration_ms": 600_000,
            "event_sequence": event_sequence,
        });
        let fixture = &fix;
        async move {
            fixture
                .request_with_body(
                    "POST",
                    "/api/v1/playback/progress",
                    Some(&fixture.admin_token),
                    body,
                )
                .await
        }
    };

    let (status, body) = post("start", 1_000, 2).await;
    assert_eq!(status, StatusCode::OK, "start body={body}");

    let (unit_before, session_before) = {
        let guard = store_arc.lock();
        (
            guard
                .unit_state(user_id, media_id, -1, -1)
                .unwrap()
                .unwrap(),
            guard.get_session(user_id, device).unwrap().unwrap(),
        )
    };
    assert_eq!(
        session_before.last_event_sequence,
        Some(2),
        "前置:start 必须把 event_sequence=2 落进 live 行"
    );
    assert_eq!(unit_before.position_ms, 1_000);

    let (status, body) = post("progress", 300_000, 1).await;
    assert_eq!(status, StatusCode::OK, "regressed body={body}");

    let guard = store_arc.lock();
    assert_eq!(
        guard
            .unit_state(user_id, media_id, -1, -1)
            .unwrap()
            .unwrap(),
        unit_before,
        "序号回退必须零写入(位置不得前进到 300000)"
    );
    let session_after = guard
        .get_session(user_id, device)
        .unwrap()
        .expect("Ignore 不得删除 live 行");
    assert_eq!(session_after, session_before, "会话行必须逐字节不变");
}

#[path = "playback_event_identity/merged_session.rs"]
mod merged_session;
#[path = "playback_event_identity/legacy_no_id_identity.rs"]
mod legacy_no_id_identity;

/// R09: a Start without `position_ms` may only reuse a resume point recorded against the same
/// physical source; after the bytes were replaced it starts from zero.
#[tokio::test]
async fn start_without_position_does_not_rebind_old_source_resume() {
    let fix = PresenceFixture::new();
    let (media_id, file) = fix.insert_media_and_ledger("movie_resume.mkv");
    let store_arc = fix.state.store();
    let user_id = store_arc.lock().user_by_token(&fix.admin_token).unwrap().unwrap().id;
    let post = |sid: String, event: &str, position: Option<i64>| {
        let mut body = json!({
            "device_id": "dev1",
            "media_item_id": media_id.to_string(),
            "file_id": file.file_id.to_string(),
            "event": event,
            "play_session_id": sid,
        });
        if let Some(p) = position {
            body["position_ms"] = json!(p);
        }
        body
    };
    let s1 = support::session::issue_session(&store_arc.lock(), user_id, "dev1", file.file_id, 1000);
    for (event, pos) in [("start", Some(0)), ("stop", Some(600_000))] {
        let (status, _) = fix
            .request_with_body("POST", "/api/v1/playback/progress", Some(&fix.admin_token), post(s1.clone(), event, pos))
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    let unit = |store: &store::Store| store.unit_state(user_id, media_id, -1, -1).unwrap().unwrap();
    assert_eq!(unit(&store_arc.lock()).position_ms, 600_000, "positive control");

    {
        let guard = store_arc.lock();
        let scope = store::library_write::WriteScope {
            library_id: file.library_id,
            config_revision: guard.library_config_revision(&file.library_id.to_string()).unwrap(),
        };
        let observation = library::SourceObservation {
            source_version: "replaced-bytes".into(),
            size_bytes: Some(1),
            modified_ns: Some(1),
            created_ns: None,
            content_digest: None,
        };
        guard.replace_file_source(&scope, file.file_id, &file.version, &observation, 5_000).unwrap();
    }
    let s2 = support::session::issue_session(&store_arc.lock(), user_id, "dev1", file.file_id, 6000);
    let (status, _) = fix
        .request_with_body("POST", "/api/v1/playback/progress", Some(&fix.admin_token), post(s2, "start", None))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(unit(&store_arc.lock()).position_ms, 0, "old-source resume must not be rebound");
}

/// R13: the historical movie 0/0 coordinates are no longer an alias of the whole movie.
#[tokio::test]
async fn movie_zero_coordinates_are_rejected_without_writes() {
    let fix = PresenceFixture::new();
    let (media_id, file) = fix.insert_media_and_ledger("movie_zero.mkv");
    let store_arc = fix.state.store();
    let user_id = store_arc.lock().user_by_token(&fix.admin_token).unwrap().unwrap().id;
    let sid = support::session::issue_session(&store_arc.lock(), user_id, "dev1", file.file_id, 1000);
    let (status, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "dev1",
                "media_item_id": media_id.to_string(),
                "file_id": file.file_id.to_string(),
                "season": 0,
                "episode": 0,
                "event": "progress",
                "position_ms": 5_000,
                "play_session_id": sid,
            }),
        )
        .await;
    assert!(!status.is_success(), "movie 0/0 must be refused, got {status}");
    let guard = store_arc.lock();
    assert!(guard.unit_state(user_id, media_id, -1, -1).unwrap().is_none());
    assert!(guard.unit_state(user_id, media_id, 0, 0).unwrap().is_none());
}

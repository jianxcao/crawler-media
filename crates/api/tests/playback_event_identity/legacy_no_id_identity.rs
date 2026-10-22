use super::*;

/// 旧客户端的行身份(只带后续步骤要用的两个 id;字段名与原 fixture 行一致)。
#[derive(Clone, Copy)]
struct LegacyRow {
    media_id: domain::MediaId,
    file_id: domain::LedgerId,
}

/// 逐步累加的 `playback_units` / `playback_sessions` 行数,用于「零新增」断言。
#[derive(Clone, Copy)]
struct LegacyCounts {
    units: i64,
    sessions: i64,
}

/// RED(task-88 必做#2/#3):无 `play_session_id` 的旧客户端事件必须按 canonical 事实落地,
/// 且 **不得**在 stale 时伪造会话。
#[tokio::test]
async fn no_id_legacy_event_is_committed_and_does_not_fabricate_session_on_stale() {
    let fix = PresenceFixture::new();
    let (_, row_a) = fix.insert_media_and_ledger("legacy_a.mkv");
    let (_, row_b) = fix.insert_media_and_ledger("legacy_b.mkv");

    let user_id = {
        let store_arc = fix.state.store();
        let guard = store_arc.lock();
        guard.user_by_token(&fix.admin_token).unwrap().unwrap().id
    };
    let row_a = LegacyRow {
        media_id: row_a.media_id,
        file_id: row_a.file_id,
    };
    let row_b = LegacyRow {
        media_id: row_b.media_id,
        file_id: row_b.file_id,
    };

    let before = legacy_unbound_progress_writes_only_unit_facts(&fix, user_id, row_a).await;
    legacy_no_id_start_then_progress_reuses_the_session(&fix, user_id, row_a, before).await;
    legacy_stale_stop_on_other_target_leaves_session_a(&fix, user_id, row_a, row_b).await;
}

/// 行为步骤 1:首个不带 id 的 progress 必须 200 + 落单元事实 + 零新增会话行。
async fn legacy_unbound_progress_writes_only_unit_facts(
    fix: &PresenceFixture,
    user_id: domain::UserId,
    row_a: LegacyRow,
) -> LegacyCounts {
    // 1. 无 `play_session_id` 的旧客户端**首个** `progress` 必须 200 + 落单元事实 + 零新增会话
    //    (§6/§16.3 ①)。它曾经是缺陷 D 的 500「保存播放状态失败」(嵌套 `BEGIN IMMEDIATE`
    //    被吞掉),复现命令:`cargo test -p api --test storage_contention`。
    //    入口是分类器 (`crates/api/src/playback_events.rs:188-190`):首个 no-id progress
    //    (`previous == None`)判 `Update`,于是 canonical 的 unbound 分支
    //    (`crates/store/src/playback/event.rs:77-107`)写单元事实并返回 `Applied`;
    //    该分支不签发会话、不写播放日志。
    let db = rusqlite::Connection::open(fix.dir.path().join("data/library.db")).unwrap();
    let sessions_before = count_rows(&db, "playback_sessions");
    let units_before = count_rows(&db, "playback_units");
    drop(db);
    let (s_stale, stale_body) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "legacy_dev",
                "media_item_id": row_a.media_id.to_string(),
                "file_id": row_a.file_id.to_string(),
                "event": "progress",
                "position_ms": 3000,
                "duration_ms": 600_000,
            }),
        )
        .await;
    assert_eq!(s_stale, StatusCode::OK, "unbound progress 必须 200(不能是 500 保存播放状态失败): {stale_body}");

    // §16.3 ① : unbound 分支仍要写单元事实。
    let store_arc = fix.state.store();
    let unit = {
        let guard = store_arc.lock();
        guard.unit_state(user_id, row_a.media_id, -1, -1).unwrap()
    };
    assert_eq!(
        unit.map(|unit| unit.position_ms),
        Some(3000),
        "unbound progress 必须写 playback_units 行"
    );
    let db = rusqlite::Connection::open(fix.dir.path().join("data/library.db")).unwrap();
    assert_eq!(count_rows(&db, "playback_units"), units_before + 1, "unbound progress 写一行单元事实");
    assert_eq!(count_rows(&db, "playback_sessions"), sessions_before, "unbound progress 不得新增会话行");
    drop(db);

    // Stale progress must not create a session
    let sess = store_arc.lock().get_session(user_id, "legacy_dev").unwrap();
    assert!(sess.is_none(), "Unbound progress without active session must not fabricate a session");
    LegacyCounts {
        units: units_before,
        sessions: sessions_before,
    }
}

/// 行为步骤 2:不带 id 的 start 只签发一个会话行;同设备后续 progress 复用它且零新增行。
async fn legacy_no_id_start_then_progress_reuses_the_session(
    fix: &PresenceFixture,
    user_id: domain::UserId,
    row_a: LegacyRow,
    before: LegacyCounts,
) {
    let sessions_before = before.sessions;
    let units_before = before.units;
    let store_arc = fix.state.store();

    // 2. Start on target A creates session and commits
    let (s_start, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "legacy_dev",
                "media_item_id": row_a.media_id.to_string(),
                "file_id": row_a.file_id.to_string(),
                "event": "start",
                "position_ms": 1000,
            }),
        )
        .await;
    assert_eq!(s_start, StatusCode::OK);

    // Session on A is active
    let sess_after_start = store_arc.lock().get_session(user_id, "legacy_dev").unwrap().unwrap();
    assert_eq!(sess_after_start.ledger_id.as_deref(), Some(&row_a.file_id.to_string()[..]));

    // 2b. §16.3 ① 正面证据:同一设备、同样不带 `play_session_id` 的后续 `progress` 复用已签发
    //     会话(canonical 的 bound 分支),必须 HTTP 200 + 位置写进 `playback_units` +
    //     `playback_sessions` **零新增行**(只更新已签发会话,不再签发)。
    let db = rusqlite::Connection::open(fix.dir.path().join("data/library.db")).unwrap();
    let sessions_after_start = count_rows(&db, "playback_sessions");
    let units_after_start = count_rows(&db, "playback_units");
    drop(db);
    assert_eq!(sessions_after_start, sessions_before + 1, "no-id start 只签发一个会话行");
    assert_eq!(units_after_start, units_before + 1, "no-id start 落一行单元事实(正控制)");

    let (s_progress, progress_body) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "legacy_dev",
                "media_item_id": row_a.media_id.to_string(),
                "file_id": row_a.file_id.to_string(),
                "event": "progress",
                "position_ms": 3000,
                "duration_ms": 600_000,
            }),
        )
        .await;
    assert_eq!(s_progress, StatusCode::OK, "no-id progress 必须 200: {progress_body}");
    let unit_after_progress = {
        let guard = store_arc.lock();
        guard.unit_state(user_id, row_a.media_id, -1, -1).unwrap()
    };
    assert_eq!(
        unit_after_progress.map(|unit| unit.position_ms),
        Some(3000),
        "§16.3 ① :no-id progress 必须把位置写进 playback_units"
    );
    let db = rusqlite::Connection::open(fix.dir.path().join("data/library.db")).unwrap();
    assert_eq!(count_rows(&db, "playback_units"), units_after_start, "progress 只更新单元事实,不新增行");
    assert_eq!(
        count_rows(&db, "playback_sessions"),
        sessions_after_start,
        "§16.3 ① :no-id progress 零新增会话行"
    );
    drop(db);
}

/// 行为步骤 3:同设备上针对另一个目标的迟到 stop 不得关闭 A 的会话。
async fn legacy_stale_stop_on_other_target_leaves_session_a(
    fix: &PresenceFixture,
    user_id: domain::UserId,
    row_a: LegacyRow,
    row_b: LegacyRow,
) {
    let store_arc = fix.state.store();

    // 3. Stale stop on target B without ID arriving on same device must not close or touch A
    let (s_stop_b, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            json!({
                "device_id": "legacy_dev",
                "media_item_id": row_b.media_id.to_string(),
                "file_id": row_b.file_id.to_string(),
                "event": "stop",
                "position_ms": 500,
            }),
        )
        .await;
    assert_eq!(s_stop_b, StatusCode::OK);

    let sess_after_mismatch = store_arc.lock().get_session(user_id, "legacy_dev").unwrap().unwrap();
    assert_eq!(sess_after_mismatch.ledger_id.as_deref(), Some(&row_a.file_id.to_string()[..]), "Mismatch stop must not close session A");
}

fn count_rows(db: &rusqlite::Connection, table: &str) -> i64 {
    db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("count {table}: {error}"))
}

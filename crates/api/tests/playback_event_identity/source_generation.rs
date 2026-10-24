use super::*;

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
            json!({ "event_sequence": crate::support::session::next_sequence(),
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
            json!({ "event_sequence": crate::support::session::next_sequence(),
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
        guard
            .unit_state(user_id, media_id, -1, -1)
            .unwrap()
            .unwrap()
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

    for event in ["progress", "stop"] {
        let (status, body) = fix
            .request_with_body(
                "POST",
                "/api/v1/playback/progress",
                Some(&fix.admin_token),
                json!({ "event_sequence": crate::support::session::next_sequence(),
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
            guard
                .unit_state(user_id, media_id, -1, -1)
                .unwrap()
                .unwrap()
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
async fn start_without_position_does_not_rebind_old_source_resume() {
    let fix = PresenceFixture::new();
    let (media_id, file) = fix.insert_media_and_ledger("movie_resume.mkv");
    let store_arc = fix.state.store();
    let user_id = store_arc
        .lock()
        .user_by_token(&fix.admin_token)
        .unwrap()
        .unwrap()
        .id;
    let post = |sid: String, event: &str, position: Option<i64>| {
        let mut body = json!({ "event_sequence": crate::support::session::next_sequence(),
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
    let s1 =
        support::session::issue_session(&store_arc.lock(), user_id, "dev1", file.file_id, 1000);
    for (event, pos) in [("start", Some(0)), ("stop", Some(600_000))] {
        let (status, _) = fix
            .request_with_body(
                "POST",
                "/api/v1/playback/progress",
                Some(&fix.admin_token),
                post(s1.clone(), event, pos),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    let unit = |store: &store::Store| {
        store
            .unit_state(user_id, media_id, -1, -1)
            .unwrap()
            .unwrap()
    };
    assert_eq!(
        unit(&store_arc.lock()).position_ms,
        600_000,
        "positive control"
    );

    {
        let guard = store_arc.lock();
        let scope = store::library_write::WriteScope {
            library_id: file.library_id,
            config_revision: guard
                .library_config_revision(&file.library_id.to_string())
                .unwrap(),
        };
        let observation = library::SourceObservation {
            source_version: "replaced-bytes".into(),
            size_bytes: Some(1),
            modified_ns: Some(1),
            created_ns: None,
            content_digest: None,
        };
        guard
            .replace_file_source(&scope, file.file_id, &file.version, &observation, 5_000)
            .unwrap();
    }
    let s2 =
        support::session::issue_session(&store_arc.lock(), user_id, "dev1", file.file_id, 6000);
    let (status, _) = fix
        .request_with_body(
            "POST",
            "/api/v1/playback/progress",
            Some(&fix.admin_token),
            post(s2, "start", None),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        unit(&store_arc.lock()).position_ms,
        0,
        "old-source resume must not be rebound"
    );
}

#[path = "management/common.rs"]
mod common;
#[path = "playback_visibility_revocation/fixture.rs"]
mod fixture;

use axum::http::StatusCode;
use domain::{MediaId, MediaKind, UnitKey, User, UserId, UserRole};
use fixture::*;
use media_server::provider::{
    MediaServerProvider, PlaybackClientInfo, PlaybackEvent, PlaybackEventIdentity,
};
use serde_json::{Value, json};
use store::library_write::WriteScope;
use store::playback::{MediaMergeRequest, PlaySessionIdState, PlaybackMutation};

#[tokio::test]
async fn revoke_device_closes_pending_session_ids_and_blocks_progress_and_renewal() {
    let env = TestEnv::new();
    let media_id = seed_media(&env.state.store(), MediaKind::Movie, "Revoke Movie");
    let file = publish_file(
        &env.state.store(),
        media_id,
        &env.root().join("data/library/movies/Revoke.mkv"),
        &[UnitKey::WHOLE],
    );
    {
        let s = env.state.store();
        s.set_user_token(env.admin_id, "dev-tok").unwrap();
        s.bind_playback_credential(env.admin_id, "tv-1", "dev-tok")
            .unwrap();
    }
    let session_res = env
        .call(
            "POST",
            "/api/v1/playback/sessions",
            "management-secret",
            json!({ "media_item_id": media_id, "device_id": "tv-1" }),
        )
        .await;
    assert_eq!(session_res.status(), StatusCode::OK);
    let sid = common::json_data(session_res).await["play_session_id"]
        .as_str()
        .unwrap()
        .to_string();

    let revoke_res = env
        .call(
            "DELETE",
            &format!("/api/v1/playback/devices/tv-1?user_id={}", env.admin_id),
            "management-secret",
            Value::Null,
        )
        .await;
    assert_eq!(revoke_res.status(), StatusCode::OK);

    let binding = env
        .state
        .store()
        .play_session_binding(env.admin_id, "tv-1", &sid)
        .unwrap()
        .unwrap();
    assert_eq!(binding.state, PlaySessionIdState::Closed);

    let prog = env
        .call(
            "POST",
            "/api/v1/playback/progress",
            "management-secret",
            json!({
                "media_item_id": media_id, "file_id": file.file_id,
                "device_id": "tv-1", "play_session_id": sid,
                "event": "progress", "event_sequence": common::next_playback_sequence(),
                "position_ms": 1_000,
            }),
        )
        .await;
    assert_eq!(prog.status(), StatusCode::FORBIDDEN);
    assert_eq!(common::json_error(prog).await["code"], "playback.revoked");

    let renew = env
        .call(
            "POST",
            "/api/v1/playback/sessions",
            "management-secret",
            json!({
                "media_item_id": media_id, "device_id": "tv-1",
                "resume_from_play_session_id": sid,
            }),
        )
        .await;
    assert_ne!(renew.status(), StatusCode::OK);
    let fresh = env
        .call(
            "POST",
            "/api/v1/playback/sessions",
            "management-secret",
            json!({ "media_item_id": media_id, "device_id": "tv-1" }),
        )
        .await;
    assert_eq!(fresh.status(), StatusCode::FORBIDDEN);
    assert_eq!(common::json_error(fresh).await["code"], "playback.revoked");
}

#[tokio::test]
async fn revoke_device_during_media_merge_returns_conflict_after_app_revocation() {
    let env = TestEnv::new();
    let survivor = seed_media(&env.state.store(), MediaKind::Movie, "Survivor");
    let loser = seed_media(&env.state.store(), MediaKind::Movie, "Loser");
    let file = publish_file(
        &env.state.store(),
        loser,
        &env.root().join("data/library/movies/Loser.mkv"),
        &[UnitKey::WHOLE],
    );
    publish_file(
        &env.state.store(),
        survivor,
        &env.root().join("data/library/movies/Survivor.mkv"),
        &[UnitKey::WHOLE],
    );
    {
        let s = env.state.store();
        s.set_user_token(env.admin_id, "merge-dev-tok").unwrap();
        s.bind_playback_credential(env.admin_id, "tv-merge", "merge-dev-tok")
            .unwrap();
    }
    let session_res = env
        .call(
            "POST",
            "/api/v1/playback/sessions",
            "management-secret",
            json!({ "media_item_id": loser, "device_id": "tv-merge" }),
        )
        .await;
    let sid = common::json_data(session_res).await["play_session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let start_res = env
        .call(
            "POST",
            "/api/v1/playback/progress",
            "management-secret",
            json!({
                "media_item_id": loser, "file_id": file.file_id,
                "device_id": "tv-merge", "play_session_id": sid,
                "event": "start", "event_sequence": common::next_playback_sequence(),
                "position_ms": 500,
            }),
        )
        .await;
    assert_eq!(start_res.status(), StatusCode::OK);

    env.state
        .store()
        .begin_media_merge(
            &MediaMergeRequest {
                request_id: "00000000-0000-0000-0001-0000000000b1".into(),
                survivor,
                loser,
                survivor_revision: 0,
                loser_revision: 0,
                refresh_targets: Default::default(),
            },
            1_000,
        )
        .unwrap();

    let revoke_res = env
        .call(
            "DELETE",
            &format!("/api/v1/playback/devices/tv-merge?user_id={}", env.admin_id),
            "management-secret",
            Value::Null,
        )
        .await;
    assert_eq!(revoke_res.status(), StatusCode::CONFLICT);
    assert_eq!(
        common::json_error(revoke_res).await["code"],
        "playback.protected"
    );
    assert!(
        env.state
            .store()
            .is_device_revoked(env.admin_id, "tv-merge")
            .unwrap()
    );

    let prog = env
        .call(
            "POST",
            "/api/v1/playback/progress",
            "management-secret",
            json!({
                "media_item_id": loser, "file_id": file.file_id,
                "device_id": "tv-merge", "play_session_id": sid,
                "event": "progress", "event_sequence": common::next_playback_sequence(),
                "position_ms": 1_500,
            }),
        )
        .await;
    assert_eq!(prog.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn marks_and_resume_return_404_item_missing_before_coords_and_leak_checks() {
    let env = TestEnv::new();
    let (_member, member_tok) = seed_member(&env.state.store(), "vis-member");
    let s = env.state.store();
    let hidden_mov_dir = env.root().join("hidden-mov");
    let hidden_tv_dir = env.root().join("hidden-tv");
    let pend_mov_dir = env.root().join("pend-mov");
    let pend_tv_dir = env.root().join("pend-tv");
    create_lib(
        &s,
        MediaKind::Movie,
        "HiddenMov",
        &hidden_mov_dir,
        "selected",
    );
    create_lib(&s, MediaKind::Tv, "HiddenTv", &hidden_tv_dir, "selected");
    let pend_mov_lib = create_lib(&s, MediaKind::Movie, "PendMov", &pend_mov_dir, "everyone");
    let pend_tv_lib = create_lib(&s, MediaKind::Tv, "PendTv", &pend_tv_dir, "everyone");

    let hidden_mov = seed_media(&s, MediaKind::Movie, "HiddenMov");
    let hidden_tv = seed_media(&s, MediaKind::Tv, "HiddenTv");
    let pend_mov = seed_media(&s, MediaKind::Movie, "PendMov");
    let pend_tv = seed_media(&s, MediaKind::Tv, "PendTv");

    let hidden_mov_file = publish_file(
        &s,
        hidden_mov,
        &hidden_mov_dir.join("m.mkv"),
        &[UnitKey::WHOLE],
    );
    let hidden_tv_file = publish_file(
        &s,
        hidden_tv,
        &hidden_tv_dir.join("multi.mkv"),
        &[UnitKey::new(1, 1), UnitKey::new(1, 2)],
    );
    let pend_mov_file = reserve_pending(
        &s,
        pend_mov_lib,
        pend_mov,
        &pend_mov_dir.join("p.mkv"),
        &[UnitKey::WHOLE],
    );
    let pend_tv_file = reserve_pending(
        &s,
        pend_tv_lib,
        pend_tv,
        &pend_tv_dir.join("ptv.mkv"),
        &[UnitKey::new(1, 1), UnitKey::new(1, 2)],
    );

    for target in [hidden_mov, pend_mov, hidden_tv, pend_tv, MediaId::new()] {
        for (m, uri, body) in [
            (
                "GET",
                format!("/api/v1/playback/marks?media_item_id={target}&season_number=1"),
                Value::Null,
            ),
            (
                "POST",
                "/api/v1/playback/marks".into(),
                json!({"media_item_id": target, "season_number": 1, "episode_number": 1, "played": true}),
            ),
            (
                "GET",
                format!("/api/v1/playback/resume?media_item_id={target}&season_number=1"),
                Value::Null,
            ),
            (
                "GET",
                format!("/api/v1/playback/resume?media_item_id={target}"),
                Value::Null,
            ),
        ] {
            assert_item_missing(
                env.call(m, &uri, &member_tok, body).await,
                &format!("{m} {uri}"),
            )
            .await;
        }
    }

    for (target, fid) in [
        (hidden_mov, hidden_mov_file.file_id),
        (hidden_tv, hidden_tv_file.file_id),
        (pend_mov, pend_mov_file),
        (pend_tv, pend_tv_file),
        (hidden_tv, hidden_mov_file.file_id),
    ] {
        for ep in ["/api/v1/playback/sessions", "/api/v1/playback/decide"] {
            let r = env
                .call(
                    "POST",
                    ep,
                    &member_tok,
                    json!({"media_item_id": target, "file_id": fid, "device_id": "d1"}),
                )
                .await;
            assert_item_missing(r, &format!("{ep} {target}/{fid}")).await;
        }
    }
}

#[tokio::test]
async fn visible_unplayable_tv_resume_returns_200_and_visible_movie_coords_return_400() {
    let env = TestEnv::new();
    let (member, member_tok) = seed_member(&env.state.store(), "vis-ok");
    let tv_dir = env.root().join("vis-tv");
    let mov_dir = env.root().join("vis-mov");
    std::fs::create_dir_all(&tv_dir).unwrap();
    std::fs::create_dir_all(&mov_dir).unwrap();
    let s = env.state.store();
    let tv_lib: domain::LibraryId = s
        .create_library(
            MediaKind::Tv,
            "VisTv",
            &[tv_dir.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap()
        .id
        .parse()
        .unwrap();
    s.create_library(
        MediaKind::Movie,
        "VisMov",
        &[mov_dir.to_str().unwrap()],
        "everyone",
        true,
        &[],
    )
    .unwrap();

    let tv_id = seed_media(&s, MediaKind::Tv, "MissingTv");
    let tv_file = publish_file(&s, tv_id, &tv_dir.join("S01E01.mkv"), &[UnitKey::new(1, 1)]);
    let mut cov = s.file_coverage_by_id(tv_file.file_id).unwrap().unwrap();
    cov.file.presence = domain::FilePresence::Missing;
    let scope = WriteScope {
        library_id: tv_lib,
        config_revision: s.library_config_revision(&tv_lib.to_string()).unwrap(),
    };
    s.publish_files(
        &scope,
        &[store::library_write::FilePublication {
            file: &cov.file,
            units: &cov.units,
            expected_version: Some(&tv_file.version),
            nfo_observation: None,
            probe_required: false,
        }],
        2_000,
    )
    .unwrap();
    s.mutate_playback(
        tv_id,
        &PlaybackMutation::marks(member, 1, 1, Some(true), None),
        3_000,
    )
    .unwrap();

    let mov_id = seed_media(&s, MediaKind::Movie, "VisMov");
    publish_file(&s, mov_id, &mov_dir.join("m.mkv"), &[UnitKey::WHOLE]);

    let r1 = env
        .call(
            "GET",
            &format!("/api/v1/playback/resume?media_item_id={tv_id}"),
            &member_tok,
            Value::Null,
        )
        .await;
    assert_eq!(r1.status(), StatusCode::OK);
    let r2 = env
        .call(
            "GET",
            &format!(
                "/api/v1/playback/resume?media_item_id={tv_id}&season_number=1&episode_number=1"
            ),
            &member_tok,
            Value::Null,
        )
        .await;
    assert_eq!(r2.status(), StatusCode::OK);
    assert_eq!(common::json_data(r2).await["played"], true);

    for (m, uri, body) in [
        (
            "GET",
            format!("/api/v1/playback/marks?media_item_id={mov_id}&season_number=1"),
            Value::Null,
        ),
        (
            "POST",
            "/api/v1/playback/marks".into(),
            json!({"media_item_id": mov_id, "season_number": 1, "played": true}),
        ),
        (
            "GET",
            format!("/api/v1/playback/resume?media_item_id={mov_id}&season_number=1"),
            Value::Null,
        ),
    ] {
        let resp = env.call(m, &uri, &member_tok, body).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(common::json_error(resp).await["code"], "playback.invalid");
    }
}

#[tokio::test]
async fn jellyfin_user_marks_and_get_user_enforce_visibility_and_enabled_status() {
    let env = TestEnv::new();
    let (member, _) = seed_member(&env.state.store(), "jf-member");
    let hidden_dir = env.root().join("jf-hidden");
    std::fs::create_dir_all(&hidden_dir).unwrap();
    let s = env.state.store();
    s.create_library(
        MediaKind::Movie,
        "JfHidden",
        &[hidden_dir.to_str().unwrap()],
        "selected",
        true,
        &[],
    )
    .unwrap();
    let hidden_mov = seed_media(&s, MediaKind::Movie, "JfHiddenMov");
    publish_file(&s, hidden_mov, &hidden_dir.join("h.mkv"), &[UnitKey::WHOLE]);
    let provider = api::media_server_provider::ApiServerProvider::new(env.state.clone());
    let work_id = domain::TypedItemId::Work(hidden_mov).to_wire_string();
    let marked = provider
        .set_user_marks(member, &work_id, Some(true), None)
        .await
        .unwrap();
    assert!(
        !marked,
        "invisible item must resolve to Ok(false) (404), not 500"
    );

    let disabled = UserId::new();
    s.insert_user_with_password(
        &User {
            id: disabled,
            login: "disabled-u".into(),
            enabled: false,
            role: UserRole::Member,
        },
        "pw",
    )
    .unwrap();
    assert!(provider.get_user(disabled).await.unwrap().is_none());
}

#[tokio::test]
async fn jellyfin_get_user_treats_pending_user_removal_as_inactive() {
    let env = TestEnv::new();
    let (member, _) = seed_member(&env.state.store(), "jf-removing");
    let provider = api::media_server_provider::ApiServerProvider::new(env.state.clone());
    assert!(provider.get_user(member).await.unwrap().is_some());
    env.state
        .store()
        .put_setting(&format!("user_removal.{member}"), "{}")
        .unwrap();
    assert!(provider.get_user(member).await.unwrap().is_none());
}

#[tokio::test]
async fn jellyfin_session_events_handle_missing_invisible_revoked_and_inactive_without_500() {
    let env = TestEnv::new();
    let (member, member_tok) = seed_member(&env.state.store(), "jf-ev-member");
    let s = env.state.store();
    let hid_dir = env.root().join("jf-ev-hid");
    let pub_dir = env.root().join("jf-ev-pub");
    create_lib(&s, MediaKind::Movie, "JfEvHid", &hid_dir, "selected");
    let pub_lib = create_lib(&s, MediaKind::Movie, "JfEvPub", &pub_dir, "everyone");
    let (hid_m, pend_m, pub_m) = (
        seed_media(&s, MediaKind::Movie, "HidM"),
        seed_media(&s, MediaKind::Movie, "PendM"),
        seed_media(&s, MediaKind::Movie, "PubM"),
    );
    publish_file(&s, hid_m, &hid_dir.join("h.mkv"), &[UnitKey::WHOLE]);
    reserve_pending(
        &s,
        pub_lib,
        pend_m,
        &pub_dir.join("p.mkv"),
        &[UnitKey::WHOLE],
    );
    publish_file(&s, pub_m, &pub_dir.join("v.mkv"), &[UnitKey::WHOLE]);

    for m in [hid_m, pend_m, MediaId::new()] {
        let wid = domain::TypedItemId::Work(m).to_wire_string();
        for ep in [
            "/Sessions/Playing",
            "/Sessions/Playing/Progress",
            "/Sessions/Playing/Stopped",
        ] {
            let r = env
                .call(
                    "POST",
                    ep,
                    &member_tok,
                    json!({"ItemId": wid, "PositionTicks": 10_000}),
                )
                .await;
            assert_eq!(r.status(), StatusCode::NO_CONTENT, "{ep} {m}");
        }
    }
    s.set_user_token(member, "rev-tok").unwrap();
    s.bind_playback_credential(member, "rev-dev", "rev-tok")
        .unwrap();
    s.revoke_device(member, "rev-dev").unwrap();
    let disabled = UserId::new();
    s.insert_user_with_password(
        &User {
            id: disabled,
            login: "dis-ev".into(),
            enabled: false,
            role: UserRole::Member,
        },
        "pw",
    )
    .unwrap();
    let (removing, _) = seed_member(&s, "rem-ev");
    s.put_setting(&format!("user_removal.{removing}"), "{}")
        .unwrap();
    let provider = api::media_server_provider::ApiServerProvider::new(env.state.clone());
    let pub_wid = domain::TypedItemId::Work(pub_m).to_wire_string();
    let id = PlaybackEventIdentity {
        play_session_id: None,
        event_sequence: None,
        media_source_id: None,
    };
    let cli = |d: &str| PlaybackClientInfo {
        device_id: d.into(),
        device_name: None,
        client: None,
        client_version: None,
    };
    for (u, d) in [
        (member, "rev-dev"),
        (disabled, "d-dis"),
        (removing, "d-rem"),
    ] {
        provider
            .report_playback_event(
                u,
                &pub_wid,
                Some(1_000),
                false,
                cli(d),
                PlaybackEvent::Playing,
                id.clone(),
            )
            .await
            .unwrap();
    }
    assert!(
        s.active_sessions(i64::MAX / 2, i64::MAX / 2)
            .unwrap()
            .is_empty()
    );
    for (u, m) in [
        (member, hid_m),
        (member, pend_m),
        (member, pub_m),
        (disabled, pub_m),
        (removing, pub_m),
    ] {
        assert!(s.unit_rows(u, m).unwrap().is_empty());
    }
}

#[tokio::test]
async fn revoke_device_retry_after_merge_clears_sessions_and_activity() {
    let env = TestEnv::new();
    let s = env.state.store();
    let (surv, loser) = (
        seed_media(&s, MediaKind::Movie, "Surv"),
        seed_media(&s, MediaKind::Movie, "Loser"),
    );
    let lf = publish_file(
        &s,
        loser,
        &env.root().join("data/library/movies/LoserRetry.mkv"),
        &[UnitKey::WHOLE],
    );
    publish_file(
        &s,
        surv,
        &env.root().join("data/library/movies/SurvRetry.mkv"),
        &[UnitKey::WHOLE],
    );
    bind_device(&s, env.admin_id, "tv-retry", "retry-dev-tok");
    let sids = [
        issue_session(&env, loser, "tv-retry").await,
        issue_session(&env, loser, "tv-retry").await,
    ];
    let start = env
        .call(
            "POST",
            "/api/v1/playback/progress",
            "management-secret",
            json!({
                "media_item_id": loser, "file_id": lf.file_id, "device_id": "tv-retry",
                "play_session_id": sids[0], "event": "start",
                "event_sequence": common::next_playback_sequence(), "position_ms": 500
            }),
        )
        .await;
    assert_eq!(start.status(), StatusCode::OK);
    let op = s
        .begin_media_merge(
            &MediaMergeRequest {
                request_id: "00000000-0000-0000-0001-0000000000c1".into(),
                survivor: surv,
                loser,
                survivor_revision: 0,
                loser_revision: 0,
                refresh_targets: Default::default(),
            },
            1_000,
        )
        .unwrap();
    let url = format!("/api/v1/playback/devices/tv-retry?user_id={}", env.admin_id);
    let first = env
        .call("DELETE", &url, "management-secret", Value::Null)
        .await;
    assert_eq!(first.status(), StatusCode::CONFLICT);
    assert_eq!(
        common::json_error(first).await["code"],
        "playback.protected"
    );
    assert_eq!(
        s.advance_media_merge(&op, 1_200).unwrap().state,
        "succeeded"
    );
    let retry = env
        .call("DELETE", &url, "management-secret", Value::Null)
        .await;
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(common::json_data(retry).await["revoked"], true);
    for sid in &sids {
        assert_eq!(
            s.play_session_binding(env.admin_id, "tv-retry", sid)
                .unwrap()
                .unwrap()
                .state,
            PlaySessionIdState::Closed
        );
    }
    assert!(s.get_session(env.admin_id, "tv-retry").unwrap().is_none());
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(
        s.device_was_admin_ended(env.admin_id, "tv-retry", now_secs + 1, 60)
            .unwrap()
    );
    let act = env
        .call(
            "GET",
            "/api/v1/playback/activity",
            "management-secret",
            Value::Null,
        )
        .await;
    assert_eq!(act.status(), StatusCode::OK);
    assert!(
        !common::json_data(act).await["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["device_id"] == "tv-retry")
    );
}

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;
use store::test_support::{FixtureFile, publish_fixture_file};

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

async fn seed_test_library(app: &axum::Router, name: &str, dir: &std::path::Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/libraries",
            Some("management-secret"),
            serde_json::json!({
                "kind": "movie",
                "name": name,
                "root_paths": [dir.to_str().unwrap()],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = json_body(res).await;
    body["data"]["id"].as_str().unwrap().to_string()
}

fn seed_test_ledger(tmp: &tempfile::TempDir, lib_dir: &std::path::Path) -> domain::MediaId {
    let media_id = domain::MediaId::new();
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: domain::MediaKind::Movie,
            title: "Movie A".into(),
            year: Some(1999),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            bangumi_id: None,
            anilist_id: None,
            tvdb_id: None,
        })
        .unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(domain::LedgerId::new()),
            media_id,
            path: lib_dir.join("Movie.A.1999.mkv").to_str().unwrap().into(),
            season: None,
            episode: None,
            resolution: Some("1080p".into()),
            codec: Some("H264".into()),
            hdr: None,
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap();
    media_id
}

#[tokio::test]
async fn empty_library_history_clear_preserves_other_library() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let lib_a_id = seed_test_library(&app, "Movie Library A", &tmp.path().join("lib_a")).await;
    let _ = lib_a_id;
    let media_id = seed_test_ledger(&tmp, &tmp.path().join("lib_a"));
    let lib_b_id = seed_test_library(&app, "Movie Library B", &tmp.path().join("lib_b")).await;

    // 2. 模拟用户上报该属于 lib_a 的媒体播放进度
    let session =
        super::playback::open_play_session(&app, &media_id.to_string(), "test-device-1").await;
    let progress_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/progress",
            Some("management-secret"),
            serde_json::json!({ "event_sequence": crate::common::next_playback_sequence(),
                "device_id": "test-device-1",
                "media_item_id": media_id.to_string(),
                "play_session_id": session,
                "event": "start",
                "position_ms": 120000,
                "duration_ms": 7200000,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(progress_res.status(), StatusCode::OK);

    // 3. 针对空的 lib_b 发起历史清理：绝不能误删 lib_a 的进度！
    let clear_res = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/playback/history?scope=library&library_id={lib_b_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(clear_res.status(), StatusCode::OK);
    let clear_data: Value = json_body(clear_res).await;
    assert_eq!(
        clear_data["data"]["deleted_states"], 0,
        "空库清理应删除 0 条记录，绝不能变成全删"
    );

    // 4. 验证 lib_a 的进度依然存在
    let admin_id = domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let unit = store
        .unit_state(admin_id, media_id, -1, -1)
        .unwrap()
        .expect("lib_a 媒体进度必须保留");
    assert_eq!(unit.position_ms, 120000, "原进度必须被完好保留");
}

#[tokio::test]
async fn invalid_scope_or_id_rejected_without_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    // 非法 scope
    let res = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/history?scope=bogus",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // scope=item 但缺失或非法 media_item_id
    let res = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/history?scope=item&media_item_id=not-a-uuid",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // scope=library 但 library 不存在
    let res = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/history?scope=library&library_id=nonexistent",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn clear_history_hides_inaccessible_library_and_returns_conflict_on_merging_media() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let (_member_id, member_token) = create_member(&app, "hist-member").await;
    let hidden_dir = tmp.path().join("hidden-lib");
    std::fs::create_dir_all(&hidden_dir).unwrap();
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let hidden_lib = store
        .create_library(
            domain::MediaKind::Movie,
            "Hidden",
            &[hidden_dir.to_str().unwrap()],
            "selected",
            true,
            &[],
        )
        .unwrap();

    let hidden_res = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!(
                "/api/v1/playback/history?scope=library&library_id={}",
                hidden_lib.id
            ),
            Some(&member_token),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(hidden_res.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_error(hidden_res).await["code"], "library.missing");

    let merge_dir_a = tmp.path().join("merge-a");
    let merge_dir_b = tmp.path().join("merge-b");
    let _ = seed_test_library(&app, "Merge Lib A", &merge_dir_a).await;
    let _ = seed_test_library(&app, "Merge Lib B", &merge_dir_b).await;
    let survivor = seed_test_ledger(&tmp, &merge_dir_a);
    let loser = seed_test_ledger(&tmp, &merge_dir_b);
    let admin_id = store.admin_user_id().unwrap();
    store
        .mutate_playback(
            loser,
            &store::playback::PlaybackMutation::marks(admin_id, -1, -1, Some(true), None),
            500,
        )
        .unwrap();
    store
        .begin_media_merge(
            &store::playback::MediaMergeRequest {
                request_id: "00000000-0000-0000-0001-0000000000c1".into(),
                survivor,
                loser,
                survivor_revision: 0,
                loser_revision: 0,
                refresh_targets: Default::default(),
            },
            1_000,
        )
        .unwrap();

    let merge_res = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/playback/history?scope=item&media_item_id={loser}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(merge_res.status(), StatusCode::CONFLICT);
    assert_eq!(json_error(merge_res).await["code"], "playback.protected");
}

struct SeqClock(std::sync::atomic::AtomicI64);

impl api::playback_events::PlaybackClock for SeqClock {
    fn now_ms(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn app_with_clock(
    tmp: &tempfile::TempDir,
    clock: Arc<SeqClock>,
) -> (axum::Router, Arc<api::Store>) {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let st = state(tmp.path(), fetcher, downloader).with_playback_clock(clock);
    let store = st.store();
    (router(st), store)
}

async fn record_watch(
    app: &axum::Router,
    clock: &SeqClock,
    at_secs: i64,
    media_id: &str,
    episode: Option<(u32, u32)>,
    device: &str,
) {
    clock
        .0
        .store(at_secs * 1000, std::sync::atomic::Ordering::SeqCst);
    let mut session_body = serde_json::json!({
        "media_item_id": media_id,
        "device_id": device,
    });
    if let Some((season, ep)) = episode {
        session_body["season_number"] = serde_json::json!(season);
        session_body["episode_number"] = serde_json::json!(ep);
    }
    let open = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/sessions",
            Some("management-secret"),
            session_body,
        ))
        .await
        .unwrap();
    assert_eq!(open.status(), StatusCode::OK);
    let sid = json_data(open).await["play_session_id"]
        .as_str()
        .unwrap()
        .to_string();
    for (event, offset_ms, pos_ms) in [("start", 0, 120_000), ("stop", 30_000, 150_000)] {
        clock.0.store(
            at_secs * 1000 + offset_ms,
            std::sync::atomic::Ordering::SeqCst,
        );
        let mut ev = serde_json::json!({
            "event_sequence": crate::common::next_playback_sequence(),
            "device_id": device,
            "media_item_id": media_id,
            "play_session_id": sid,
            "event": event,
            "position_ms": pos_ms,
            "duration_ms": 2_400_000,
        });
        if let Some((season, ep)) = episode {
            ev["season_number"] = serde_json::json!(season);
            ev["episode_number"] = serde_json::json!(ep);
        }
        let resp = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/playback/progress",
                Some("management-secret"),
                ev,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
    let metric = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/metrics",
            Some("management-secret"),
            serde_json::json!({
                "media_item_id": media_id,
                "tier": 1,
                "engine": "native",
                "watched_ms": 30_000,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(metric.status(), StatusCode::OK);
}

fn seed_tv_library_and_show(
    store: &api::Store,
    dir: &std::path::Path,
    title: &str,
    sub_user: Option<domain::UserId>,
) -> (
    String,
    domain::MediaId,
    Option<domain::SubscribeId>,
    Option<String>,
) {
    std::fs::create_dir_all(dir).unwrap();
    let lib = store
        .create_library(
            domain::MediaKind::Tv,
            title,
            &[dir.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media_id = domain::MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: domain::MediaKind::Tv,
            title: title.into(),
            year: Some(2025),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some(format!("99-{title}")),
            douban_id: None,
            bangumi_id: None,
            anilist_id: None,
            tvdb_id: None,
        })
        .unwrap();
    let file_path = dir.join(format!("{title}.S01E01.mkv"));
    std::fs::write(&file_path, b"finite video").unwrap();
    store::test_support::publish_fixture_file_units(
        store,
        &store::test_support::FixtureFile {
            file_id: Some(domain::LedgerId::new()),
            media_id,
            path: file_path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            resolution: Some("1080p".into()),
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            ..Default::default()
        },
        &[domain::UnitKey::new(1, 1)],
    )
    .unwrap();
    let mut sub_info = (None, None);
    if let Some(user_id) = sub_user {
        let filter_id = domain::FilterId::new();
        store
            .insert_filter(&domain::Filter::new(filter_id, "all", vec![]))
            .unwrap();
        let sub_id = domain::SubscribeId::new();
        store
            .insert_subscribe(&domain::Subscribe {
                id: sub_id,
                user_id,
                media_id,
                coverage: domain::Coverage::Tv {
                    season: 1,
                    episode_from: 1,
                    episode_to: Some(12),
                },
                fetch_mode: domain::FetchMode::Search,
                filter_id,
                wash_cut: false,
                wash_cut_filter_id: None,
                keep_old_versions: false,
                full_season_pack: false,
                downloader_id: None,
                library_id: lib.id.parse().ok(),
                tracking_state: "active".into(),
                follow_future: false,
                search_interval_secs: 1800,
            })
            .unwrap();
        sub_info = (Some(sub_id), Some(filter_id.to_string()));
    }
    (lib.id, media_id, sub_info.0, sub_info.1)
}

fn seed_movie_in_store(store: &api::Store, dir: &std::path::Path, title: &str) -> domain::MediaId {
    std::fs::create_dir_all(dir).unwrap();
    let _ = store
        .create_library(
            domain::MediaKind::Movie,
            title,
            &[dir.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media_id = domain::MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: domain::MediaKind::Movie,
            title: title.into(),
            year: Some(1999),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            bangumi_id: None,
            anilist_id: None,
            tvdb_id: None,
        })
        .unwrap();
    let file_path = dir.join(format!("{title}.1999.mkv"));
    std::fs::write(&file_path, b"finite video").unwrap();
    publish_fixture_file(
        store,
        &FixtureFile {
            file_id: Some(domain::LedgerId::new()),
            media_id,
            path: file_path.display().to_string(),
            resolution: Some("1080p".into()),
            codec: Some("H264".into()),
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            ..Default::default()
        },
    )
    .unwrap();
    media_id
}

async fn get_up_next_ids(app: &axum::Router) -> Vec<String> {
    let resp = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/playback/up-next",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    json_data(resp).await["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["media_item_id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn clear_history_since_uses_unix_seconds_and_preserves_older_records() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = Arc::new(SeqClock(std::sync::atomic::AtomicI64::new(
        1_000_000 * 1000,
    )));
    let (app, store) = app_with_clock(&tmp, clock.clone());
    let older = seed_movie_in_store(&store, &tmp.path().join("movies"), "MovieA");
    let newer = seed_movie_in_store(&store, &tmp.path().join("movies-2"), "MovieB");

    record_watch(&app, &clock, 1_000_000, &older.to_string(), None, "dev-old").await;
    record_watch(&app, &clock, 2_000_000, &newer.to_string(), None, "dev-new").await;

    for invalid in ["not-a-number", "-5"] {
        let bad = app
            .clone()
            .oneshot(request(
                "DELETE",
                &format!("/api/v1/playback/history?scope=all&since={invalid}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST, "since={invalid}");
    }

    let clear = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/history?scope=all&since=1500000",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(clear.status(), StatusCode::OK);
    let cleared = json_data(clear).await;
    assert!(cleared["deleted_states"].as_i64().unwrap() >= 2);
    assert_eq!(cleared["deleted_metrics"], 1);

    assert_eq!(get_up_next_ids(&app).await, vec![older.to_string()]);
    let history = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/playback/history",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let entries = json_data(history).await["entries"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["media"]["media_item_id"], older.to_string());
}

#[tokio::test]
async fn clear_history_dismisses_subscribed_tv_from_up_next_until_replayed_or_resubscribed() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = Arc::new(SeqClock(std::sync::atomic::AtomicI64::new(
        1_700_000_000 * 1000,
    )));
    let (app, store) = app_with_clock(&tmp, clock.clone());
    let admin = store.admin_user_id().unwrap();
    let (lib_a, show_a, sub_a, filter_a) =
        seed_tv_library_and_show(&store, &tmp.path().join("tv-a"), "ShowA", Some(admin));
    let (_lib_b, show_b, _, _) =
        seed_tv_library_and_show(&store, &tmp.path().join("tv-b"), "ShowB", Some(admin));

    let initial = get_up_next_ids(&app).await;
    assert_eq!(initial.len(), 2);

    // 1. 单剧清除：未播过的订阅剧也应立刻从「接下来继续」移出，且返回 >= 1。
    let clear_item = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/playback/history?scope=item&media_item_id={show_a}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(clear_item.status(), StatusCode::OK);
    assert!(
        json_data(clear_item).await["deleted_states"]
            .as_i64()
            .unwrap()
            >= 1
    );
    assert_eq!(get_up_next_ids(&app).await, vec![show_b.to_string()]);

    // 2. 带 since 窗口清除：只清窗口内播过的订阅剧（show_a 新近播放，show_b 未播过则不动）。
    record_watch(
        &app,
        &clock,
        1_700_000_500,
        &show_a.to_string(),
        Some((1, 1)),
        "dev-tv",
    )
    .await;
    assert_eq!(get_up_next_ids(&app).await.len(), 2, "重新播放后应恢复显示");
    let clear_recent = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/history?scope=all&since=1700000100",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(clear_recent.status(), StatusCode::OK);
    assert_eq!(get_up_next_ids(&app).await, vec![show_b.to_string()]);

    // 3. 全部清除 / 按库清除 / 重新订阅恢复。
    let clear_all = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/history?scope=all",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(clear_all.status(), StatusCode::OK);
    assert!(get_up_next_ids(&app).await.is_empty());

    record_watch(
        &app,
        &clock,
        1_700_001_000,
        &show_a.to_string(),
        Some((1, 1)),
        "dev-tv",
    )
    .await;
    assert_eq!(get_up_next_ids(&app).await, vec![show_a.to_string()]);
    let clear_lib = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/playback/history?scope=library&library_id={lib_a}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(clear_lib.status(), StatusCode::OK);
    assert!(get_up_next_ids(&app).await.is_empty());

    let del_sub = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/subscriptions/{}", sub_a.unwrap()),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(del_sub.status(), StatusCode::OK);
    let mut resub_body = subscribe_payload_with_filter("search", filter_a.as_deref());
    resub_body["media"] = serde_json::json!({
        "kind": "tv",
        "title": "ShowA",
        "year": 2025,
        "tmdb_id": "99-ShowA"
    });
    resub_body["coverage"] =
        serde_json::json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 12 });
    resub_body["library_id"] = serde_json::json!(lib_a);
    let resub = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            resub_body,
        ))
        .await
        .unwrap();
    assert_eq!(resub.status(), StatusCode::CREATED);
    assert_eq!(get_up_next_ids(&app).await, vec![show_a.to_string()]);
}

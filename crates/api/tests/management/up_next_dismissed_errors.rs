use std::collections::HashMap;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use api::router;
use axum::http::StatusCode;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

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

fn seed_subscribed_tv_show(
    store: &api::Store,
    dir: &std::path::Path,
    title: &str,
    user_id: domain::UserId,
) -> (String, domain::MediaId, domain::SubscribeId, String) {
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
    (lib.id, media_id, sub_id, filter_id.to_string())
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

fn hold_app_writer(store: Arc<api::Store>) -> (mpsc::SyncSender<()>, std::thread::JoinHandle<()>) {
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let handle = std::thread::spawn(move || {
        let _ = store.with_writer(api::store::Db::App, |_| {
            let _ = entered_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
            Ok(())
        });
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    (release_tx, handle)
}

async fn record_tv_watch(app: &axum::Router, clock: &SeqClock, at_secs: i64, media_id: &str) {
    clock
        .0
        .store(at_secs * 1000, std::sync::atomic::Ordering::SeqCst);
    let open = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/sessions",
            Some("management-secret"),
            json!({
                "media_item_id": media_id,
                "device_id": "dev-tv",
                "season_number": 1,
                "episode_number": 1,
            }),
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
        let resp = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/playback/progress",
                Some("management-secret"),
                json!({
                    "event_sequence": crate::common::next_playback_sequence(),
                    "device_id": "dev-tv",
                    "media_item_id": media_id,
                    "play_session_id": sid,
                    "season_number": 1,
                    "episode_number": 1,
                    "event": event,
                    "position_ms": pos_ms,
                    "duration_ms": 2_400_000,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn up_next_propagates_error_instead_of_exposing_dismissed_item_when_app_db_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = Arc::new(SeqClock(std::sync::atomic::AtomicI64::new(
        1_700_000_000 * 1000,
    )));
    let (app, store) = app_with_clock(&tmp, clock);
    let admin = store.admin_user_id().unwrap();
    let (_lib, show_id, _, _) =
        seed_subscribed_tv_show(&store, &tmp.path().join("tv-upnext"), "HiddenShow", admin);
    assert_eq!(get_up_next_ids(&app).await, vec![show_id.to_string()]);

    let dismiss = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/playback/history?scope=item&media_item_id={show_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(dismiss.status(), StatusCode::OK);
    assert!(get_up_next_ids(&app).await.is_empty());

    // 1. Exhaust all 4 Db::App reader leases: up-next must return 503, never 200 OK.
    let mut releases = Vec::new();
    let mut holders = Vec::new();
    for _ in 0..4 {
        let store_ref = store.clone();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        holders.push(std::thread::spawn(move || {
            store_ref
                .with_read_snapshot(&[api::store::Db::App], Duration::from_secs(15), || {
                    let _ = entered_tx.send(());
                    let _ = release_rx.recv_timeout(Duration::from_secs(15));
                })
                .unwrap();
        }));
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        releases.push(release_tx);
    }
    let busy_resp = app
        .clone()
        .oneshot(request(
            "GET",
            "/api/v1/playback/up-next",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    for release_tx in releases {
        let _ = release_tx.send(());
    }
    for holder in holders {
        holder.join().unwrap();
    }
    assert_eq!(busy_resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_error(busy_resp).await["code"], "runtime.busy");
    assert!(get_up_next_ids(&app).await.is_empty());

    // 2. Inject a row-scoped read fault on `user.<id>.up_next.dismissed` (BLOB in TEXT column)
    // so auth & library reads succeed and ONLY `load_dismissed` fails: up-next must return 500
    // `up_next.store`, never 200 OK exposing `show_id`.
    let dismissed_key = format!("user.{admin}.up_next.dismissed");
    store
        .with_writer(api::store::Db::App, |conn| {
            let updated = conn.execute(
                "UPDATE settings SET value = X'FF' WHERE key = ?1",
                [&dismissed_key],
            )?;
            assert_eq!(updated, 1);
            Ok(())
        })
        .unwrap();
    let fault_resp = app
        .oneshot(request(
            "GET",
            "/api/v1/playback/up-next",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(fault_resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(json_error(fault_resp).await["code"], "up_next.store");
}

#[tokio::test]
async fn resubscribe_fails_cleanly_when_clearing_dismissed_media_cannot_write_app_db() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = Arc::new(SeqClock(std::sync::atomic::AtomicI64::new(
        1_700_000_000 * 1000,
    )));
    let (app, store) = app_with_clock(&tmp, clock);
    let admin = store.admin_user_id().unwrap();
    let (lib_id, show_id, sub_id, filter_id) =
        seed_subscribed_tv_show(&store, &tmp.path().join("tv-resub"), "ResubShow", admin);

    let dismiss = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/playback/history?scope=item&media_item_id={show_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(dismiss.status(), StatusCode::OK);
    assert!(get_up_next_ids(&app).await.is_empty());

    let del_sub = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/subscriptions/{sub_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(del_sub.status(), StatusCode::OK);

    let mut resub_body = subscribe_payload_with_filter("search", Some(&filter_id));
    resub_body["media"] = json!({
        "kind": "tv",
        "title": "ResubShow",
        "year": 2025,
        "tmdb_id": "99-ResubShow",
    });
    resub_body["coverage"] =
        json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 12 });
    resub_body["library_id"] = json!(lib_id);

    let (release_tx, blocker) = hold_app_writer(store.clone());
    let blocked_resub = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            resub_body.clone(),
        ))
        .await
        .unwrap();
    let _ = release_tx.send(());
    blocker.join().unwrap();

    assert_eq!(blocked_resub.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_error(blocked_resub).await["code"], "runtime.busy");
    assert!(store.list_subscribes_for_user(admin).unwrap().is_empty());

    let retry_resub = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            resub_body,
        ))
        .await
        .unwrap();
    assert_eq!(retry_resub.status(), StatusCode::CREATED);
    assert_eq!(get_up_next_ids(&app).await, vec![show_id.to_string()]);
}

#[tokio::test]
async fn clear_all_history_leaves_library_history_intact_when_dismissed_write_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = Arc::new(SeqClock(std::sync::atomic::AtomicI64::new(
        1_700_000_000 * 1000,
    )));
    let (app, store) = app_with_clock(&tmp, clock.clone());
    let admin = store.admin_user_id().unwrap();
    let (_lib, show_id, _, _) =
        seed_subscribed_tv_show(&store, &tmp.path().join("tv-clear"), "IntactShow", admin);
    record_tv_watch(&app, &clock, 1_700_000_500, &show_id.to_string()).await;

    let (release_tx, blocker) = hold_app_writer(store.clone());
    let clear_failed = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/history",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let _ = release_tx.send(());
    blocker.join().unwrap();

    assert_eq!(clear_failed.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_error(clear_failed).await["code"], "runtime.busy");

    // Library playback history and progress must remain completely intact!
    assert!(store.unit_state(admin, show_id, 1, 1).unwrap().is_some());
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
    assert_eq!(history.status(), StatusCode::OK);
    let entries = json_data(history).await["entries"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["media"]["media_item_id"], show_id.to_string());
    assert_eq!(get_up_next_ids(&app).await, vec![show_id.to_string()]);

    // Once app.db writer is available again, clearing all history succeeds cleanly.
    let clear_ok = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/history",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(clear_ok.status(), StatusCode::OK);
    assert!(get_up_next_ids(&app).await.is_empty());
}

fn hold_library_writer(
    store: Arc<api::Store>,
) -> (mpsc::SyncSender<()>, std::thread::JoinHandle<()>) {
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let handle = std::thread::spawn(move || {
        let _ = store.with_writer(api::store::Db::Library, |_| {
            let _ = entered_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
            Ok(())
        });
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    (release_tx, handle)
}

async fn record_tv_episode_progress(
    app: &axum::Router,
    clock: &SeqClock,
    at_secs: i64,
    media_id: &str,
    episode: u32,
    stop_pos_ms: i64,
) {
    clock
        .0
        .store(at_secs * 1000, std::sync::atomic::Ordering::SeqCst);
    let open = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/sessions",
            Some("management-secret"),
            json!({
                "media_item_id": media_id,
                "device_id": format!("dev-ep-{episode}"),
                "season_number": 1,
                "episode_number": episode,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(open.status(), StatusCode::OK);
    let sid = json_data(open).await["play_session_id"]
        .as_str()
        .unwrap()
        .to_string();
    for (event, offset_ms, pos_ms) in [("start", 0, 120_000), ("stop", 30_000, stop_pos_ms)] {
        clock.0.store(
            at_secs * 1000 + offset_ms,
            std::sync::atomic::Ordering::SeqCst,
        );
        let resp = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/playback/progress",
                Some("management-secret"),
                json!({
                    "event_sequence": crate::common::next_playback_sequence(),
                    "device_id": format!("dev-ep-{episode}"),
                    "media_item_id": media_id,
                    "play_session_id": sid,
                    "season_number": 1,
                    "episode_number": episode,
                    "event": event,
                    "position_ms": pos_ms,
                    "duration_ms": 2_400_000,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn clear_all_history_rolls_back_app_dismissal_when_library_write_is_rejected_or_busy() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = Arc::new(SeqClock(std::sync::atomic::AtomicI64::new(
        1_700_000_000 * 1000,
    )));
    let (app, store) = app_with_clock(&tmp, clock);
    let admin = store.admin_user_id().unwrap();
    let (_lib, show_id, _, _) =
        seed_subscribed_tv_show(&store, &tmp.path().join("tv-rb"), "RollbackShow", admin);
    assert_eq!(get_up_next_ids(&app).await, vec![show_id.to_string()]);

    // 1. library.db writer busy (503): app.db dismissal must roll back outside admission deadline.
    let (release_tx, blocker) = hold_library_writer(store.clone());
    let busy_res = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/history",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    let _ = release_tx.send(());
    blocker.join().unwrap();
    assert_eq!(busy_res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_error(busy_res).await["code"], "runtime.busy");
    assert_eq!(get_up_next_ids(&app).await, vec![show_id.to_string()]);

    // 2. library.db Protected write barrier (409 during media merge): app.db dismissal must roll back.
    let merge_dir = tmp.path().join("movies-merge");
    std::fs::create_dir_all(&merge_dir).unwrap();
    store
        .create_library(
            domain::MediaKind::Movie,
            "MergeMovies",
            &[merge_dir.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let survivor = domain::MediaId::new();
    let loser = domain::MediaId::new();
    for (id, title) in [(survivor, "MergeSurvivor"), (loser, "MergeLoser")] {
        store
            .insert_media(&domain::Media {
                id,
                kind: domain::MediaKind::Movie,
                title: title.into(),
                year: Some(2024),
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                bangumi_id: None,
                anilist_id: None,
                tvdb_id: None,
            })
            .unwrap();
        let file_path = merge_dir.join(format!("{title}.mkv"));
        std::fs::write(&file_path, b"movie bytes").unwrap();
        store::test_support::publish_fixture_file(
            &store,
            &store::test_support::FixtureFile {
                media_id: id,
                path: file_path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
    }
    store
        .mutate_playback(
            loser,
            &store::playback::PlaybackMutation::marks(admin, -1, -1, Some(true), None),
            500,
        )
        .unwrap();
    store
        .begin_media_merge(
            &store::playback::MediaMergeRequest {
                request_id: "00000000-0000-0000-0001-0000000000d1".into(),
                survivor,
                loser,
                survivor_revision: 0,
                loser_revision: 0,
                refresh_targets: Default::default(),
            },
            1_000,
        )
        .unwrap();

    let protected_res = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/history",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(protected_res.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_error(protected_res).await["code"],
        "playback.protected"
    );
    assert_eq!(get_up_next_ids(&app).await, vec![show_id.to_string()]);
}

#[tokio::test]
async fn clear_windowed_history_preserves_up_next_when_older_watch_history_remains() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = Arc::new(SeqClock(std::sync::atomic::AtomicI64::new(1_000 * 1000)));
    let (app, store) = app_with_clock(&tmp, clock.clone());
    let admin = store.admin_user_id().unwrap();
    let show_dir = tmp.path().join("tv-window");
    let (_lib, show_id, _, _) = seed_subscribed_tv_show(&store, &show_dir, "WindowShow", admin);
    let ep2_path = show_dir.join("WindowShow.S01E02.mkv");
    std::fs::write(&ep2_path, b"finite video").unwrap();
    store::test_support::publish_fixture_file_units(
        &store,
        &store::test_support::FixtureFile {
            file_id: Some(domain::LedgerId::new()),
            media_id: show_id,
            path: ep2_path.display().to_string(),
            season: Some(1),
            episode: Some(2),
            resolution: Some("1080p".into()),
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            ..Default::default()
        },
        &[domain::UnitKey::new(1, 2)],
    )
    .unwrap();

    // Watch S01E02 to completion at t = 1000 (with S01E01 unwatched), then progress S01E01 at t = 2000.
    record_tv_episode_progress(&app, &clock, 1_000, &show_id.to_string(), 2, 2_380_000).await;
    record_tv_episode_progress(&app, &clock, 2_000, &show_id.to_string(), 1, 150_000).await;

    clock
        .0
        .store(2_500 * 1000, std::sync::atomic::Ordering::SeqCst);
    let clear_res = app
        .clone()
        .oneshot(request(
            "DELETE",
            "/api/v1/playback/history?scope=all&since=1500",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(clear_res.status(), StatusCode::OK);

    let ep1 = store.unit_state(admin, show_id, 1, 1).unwrap().unwrap();
    assert_eq!(ep1.position_ms, 0);
    assert!(!ep1.played);
    let ep2 = store.unit_state(admin, show_id, 1, 2).unwrap().unwrap();
    assert!(ep2.played, "pre-window S01E02 state must remain");
    assert!(ep2.updated_at < 1_500);
    assert_eq!(get_up_next_ids(&app).await, vec![show_id.to_string()]);
}

#[tokio::test]
async fn failed_overlapping_subscription_preserves_existing_up_next_dismissal() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = Arc::new(SeqClock(std::sync::atomic::AtomicI64::new(
        1_700_000_000 * 1000,
    )));
    let (app, store) = app_with_clock(&tmp, clock);
    let admin = store.admin_user_id().unwrap();
    let (lib_id, show_id, _sub_id, filter_id) =
        seed_subscribed_tv_show(&store, &tmp.path().join("tv-overlap"), "OverlapShow", admin);
    assert_eq!(get_up_next_ids(&app).await, vec![show_id.to_string()]);

    let dismiss = app
        .clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/playback/history?scope=item&media_item_id={show_id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(dismiss.status(), StatusCode::OK);
    assert!(get_up_next_ids(&app).await.is_empty());

    let mut overlap_body = subscribe_payload_with_filter("search", Some(&filter_id));
    overlap_body["media"] = json!({
        "kind": "tv",
        "title": "OverlapShow",
        "year": 2025,
        "tmdb_id": "99-OverlapShow",
    });
    overlap_body["coverage"] =
        json!({ "kind": "tv", "season": 1, "episode_from": 3, "episode_to": 8 });
    overlap_body["library_id"] = json!(lib_id);

    let overlap_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            overlap_body,
        ))
        .await
        .unwrap();
    assert_eq!(overlap_res.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_error(overlap_res).await["code"],
        "subscription.overlap"
    );
    assert!(get_up_next_ids(&app).await.is_empty());
}

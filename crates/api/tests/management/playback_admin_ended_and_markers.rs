use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use domain::{LedgerId, Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;
use store::test_support::{FixtureFile, publish_fixture_file, publish_fixture_file_units};

struct FakeChapters(Vec<marker::Chapter>);
impl api::marker_resolver::ChapterProbe for FakeChapters {
    fn probe(&self, _: &std::path::Path) -> Option<Vec<marker::Chapter>> {
        Some(self.0.clone())
    }
}

fn whole_tv_fixture(
    tmp: &tempfile::TempDir,
) -> (api::Store, Media, domain::FileCoverage, api::ApiState) {
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Whole TV".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let path = tmp.path().join("data/library/tv/whole.mkv");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"content").unwrap();
    let file = publish_fixture_file_units(
        &store,
        &FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            ..Default::default()
        },
        &[domain::UnitKey::WHOLE],
    )
    .unwrap();
    store
        .set_library_intro_settings(&file.library_id.to_string(), true, false)
        .unwrap();
    let row = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app_state =
        state(tmp.path(), fetcher, downloader).with_chapter_probe(Arc::new(FakeChapters(vec![
            marker::Chapter {
                start_ms: 42_000,
                end_ms: 60_000,
                title: Some("Intro".into()),
            },
        ])));
    (store, media, row, app_state)
}

#[tokio::test]
async fn whole_tv_does_not_read_or_delete_first_episode_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, media, row, app_state) = whole_tv_fixture(&tmp);
    store
        .put_media_marker(&store::StoredMediaMarker {
            media_id: media.id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(10_000),
            intro_end_ms: Some(20_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "test".into(),
            locked: false,
            updated_at: 0,
        })
        .unwrap();
    store
        .put_cached_chapters(&row.file.file_id.to_string(), &[])
        .unwrap();
    let chapters = api::marker_resolver::resolve_item_chapters(
        &app_state,
        &row,
        domain::UnitKey::WHOLE,
        &media,
        None,
    )
    .await;
    assert!(chapters.is_empty());
    let session = store
        .issue_play_session(
            store.admin_user_id().unwrap(),
            "whole-tv",
            row.file.file_id,
            media.id,
            domain::UnitKey::WHOLE,
            &row.file.version,
            0,
        )
        .unwrap();
    let response = router(app_state.clone())
        .oneshot(request(
            "GET",
            &format!("/api/v1/playback/chapters?play_session_id={session}&device_id=whole-tv"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        json_data(response).await["chapters"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    api::marker_resolver::force_refresh_item_chapters(
        &app_state,
        &row,
        domain::UnitKey::WHOLE,
        &media,
        None,
    )
    .await
    .unwrap();
    let marker = store
        .get_media_marker(media.id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(marker.intro_start_ms, Some(10_000));
}

#[tokio::test]
async fn whole_tv_chapter_detection_does_not_create_episode_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, media, row, app_state) = whole_tv_fixture(&tmp);
    let chapters = api::marker_resolver::resolve_item_chapters(
        &app_state,
        &row,
        domain::UnitKey::WHOLE,
        &media,
        None,
    )
    .await;
    assert_eq!(chapters.len(), 1);
    assert!(
        store
            .get_media_marker(media.id, Some(1), Some(1))
            .unwrap()
            .is_none()
    );
}

struct RetryingChapters {
    calls: std::sync::atomic::AtomicUsize,
    panic_first: bool,
}

impl api::marker_resolver::ChapterProbe for RetryingChapters {
    fn probe(&self, _: &std::path::Path) -> Option<Vec<marker::Chapter>> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            assert!(!self.panic_first, "simulated blocking task failure");
            return None;
        }
        Some(vec![marker::Chapter {
            start_ms: 42_000,
            end_ms: 60_000,
            title: Some("Scene".into()),
        }])
    }
}

#[tokio::test]
async fn chapter_probe_failures_are_retried_and_success_is_cached() {
    for panic_first in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        });
        let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
        let store = api::Store::open(tmp.path().join("data")).unwrap();
        let media = store
            .ensure_media(Media {
                id: MediaId::new(),
                kind: MediaKind::Tv,
                title: "Retry".into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let path = tmp.path().join("data/library/tv/retry.mkv");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"content").unwrap();
        let file = publish_fixture_file_units(
            &store,
            &FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                ..Default::default()
            },
            &[domain::UnitKey::new(1, 1)],
        )
        .unwrap();
        let row = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
        let probe = Arc::new(RetryingChapters {
            calls: std::sync::atomic::AtomicUsize::new(0),
            panic_first,
        });
        let app_state = state(tmp.path(), fetcher, downloader).with_chapter_probe(probe.clone());
        let resolve = || {
            api::marker_resolver::resolve_item_chapters(
                &app_state,
                &row,
                domain::UnitKey::new(1, 1),
                &media,
                None,
            )
        };
        assert!(resolve().await.is_empty());
        assert!(
            store
                .get_cached_chapters(&file.file_id.to_string())
                .unwrap()
                .is_none()
        );
        assert_eq!(resolve().await.len(), 1);
        assert_eq!(resolve().await.len(), 1);
        assert_eq!(probe.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn strm_skip_is_cached_without_calling_chapter_probe() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Stream".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let path = tmp.path().join("data/library/tv/stream.strm");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "https://example.com/video").unwrap();
    let file = publish_fixture_file_units(
        &store,
        &FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            ..Default::default()
        },
        &[domain::UnitKey::new(1, 1)],
    )
    .unwrap();
    let row = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
    let probe = Arc::new(RetryingChapters {
        calls: std::sync::atomic::AtomicUsize::new(0),
        panic_first: true,
    });
    let app_state = state(tmp.path(), fetcher, downloader).with_chapter_probe(probe.clone());
    for _ in 0..2 {
        assert!(
            api::marker_resolver::resolve_item_chapters(
                &app_state,
                &row,
                domain::UnitKey::new(1, 1),
                &media,
                None,
            )
            .await
            .is_empty()
        );
    }
    assert_eq!(
        store
            .get_cached_chapters(&file.file_id.to_string())
            .unwrap(),
        Some(vec![])
    );
    assert_eq!(probe.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn multiunit_cold_resolution_caches_physical_chapters_including_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Merged".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let chapters = vec![marker::Chapter {
        start_ms: 42_000,
        end_ms: 60_000,
        title: Some("Scene".into()),
    }];
    for (name, fixture) in [("has", chapters), ("empty", vec![])] {
        let path = tmp.path().join(format!("data/library/tv/{name}.mkv"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"content").unwrap();
        let file = publish_fixture_file_units(
            &store,
            &FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                ..Default::default()
            },
            &[domain::UnitKey::new(1, 1), domain::UnitKey::new(1, 2)],
        )
        .unwrap();
        let row = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
        let app_state = state(tmp.path(), fetcher.clone(), downloader.clone())
            .with_chapter_probe(Arc::new(FakeChapters(fixture.clone())));
        let result = api::marker_resolver::resolve_item_chapters(
            &app_state,
            &row,
            domain::UnitKey::new(1, 2),
            &media,
            None,
        )
        .await;
        let expected: Vec<_> = fixture
            .iter()
            .map(|chapter| marker::ChapterMarker {
                start_ms: chapter.start_ms,
                end_ms: chapter.end_ms,
                title: chapter.title.clone(),
                marker_type: None,
                synthetic: false,
            })
            .collect();
        assert_eq!(result, expected);
        assert_eq!(
            store
                .get_cached_chapters(&file.file_id.to_string())
                .unwrap()
                .unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn chapter_poll_uses_session_unit_and_rejects_changed_file_version() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Merged".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let path = tmp.path().join("data/library/tv/Merged/S01E01E02.mkv");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"one").unwrap();
    let spec = FixtureFile {
        media_id: media.id,
        path: path.display().to_string(),
        ..Default::default()
    };
    let file = publish_fixture_file_units(
        &store,
        &spec,
        &[domain::UnitKey::new(1, 1), domain::UnitKey::new(1, 2)],
    )
    .unwrap();
    let user = store.admin_user_id().unwrap();
    let session = store
        .issue_play_session(
            user,
            "chapter-test",
            file.file_id,
            media.id,
            domain::UnitKey::new(1, 2),
            &file.version,
            0,
        )
        .unwrap();
    store
        .put_cached_chapters(&file.file_id.to_string(), &[])
        .unwrap();
    for (episode, start) in [(1, 10_000), (2, 20_000)] {
        store
            .put_media_marker(&store::StoredMediaMarker {
                media_id: media.id,
                season: 1,
                episode,
                intro_start_ms: Some(start),
                intro_end_ms: Some(start + 5_000),
                outro_start_ms: None,
                outro_end_ms: None,
                source: "test".into(),
                locked: false,
                updated_at: 0,
            })
            .unwrap();
    }
    let url = format!("/api/v1/playback/chapters?play_session_id={session}&device_id=chapter-test");
    let response = app
        .clone()
        .oneshot(request("GET", &url, Some("management-secret"), Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let chapters = json_data(response).await["chapters"]
        .as_array()
        .unwrap()
        .clone();
    assert!(chapters.iter().any(|chapter| chapter["start_ms"] == 20_000));
    assert!(!chapters.iter().any(|chapter| chapter["start_ms"] == 10_000));
    std::fs::write(&path, b"changed source bytes").unwrap();
    let mut changed = file.clone();
    changed.version.source_version = library::observe_source(&path).unwrap().source_version;
    store
        .publish_files(
            &store::library_write::WriteScope {
                library_id: file.library_id,
                config_revision: store
                    .library_config_revision(&file.library_id.to_string())
                    .unwrap(),
            },
            &[store::library_write::FilePublication {
                file: &changed,
                units: &[domain::UnitKey::new(1, 1), domain::UnitKey::new(1, 2)],
                expected_version: Some(&file.version),
                nfo_observation: None,
                probe_required: false,
            }],
            0,
        )
        .unwrap();
    let response = app
        .oneshot(request("GET", &url, Some("management-secret"), Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

#[tokio::test]
async fn admin_ended_session_does_not_resurrect_on_subsequent_progress_heartbeat() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let (store, media, file_id) = seed_movie(&tmp);
    let user_id = store.admin_user_id().unwrap();
    let device_id = "device-to-end";
    let session = super::playback::open_play_session(&app, &media.id.to_string(), device_id).await;

    // 1. 发起 start 心跳建立会话
    let start_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/progress",
            Some("management-secret"),
            json!({ "event_sequence": crate::common::next_playback_sequence(),
                "media_item_id": media.id.to_string(),
                "file_id": file_id.to_string(),
                "device_id": device_id,
                "play_session_id": session,
                "event": "start",
                "position_ms": 1000,
                "duration_ms": 100000
            }),
        ))
        .await
        .unwrap();
    assert_eq!(start_res.status(), StatusCode::OK);

    // 验证会话在库中存在
    assert!(store.get_session(user_id, device_id).unwrap().is_some());

    // 2. 管理员结束该设备会话
    let end_res = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/playback/activity/sessions/{device_id}/end"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(end_res.status(), StatusCode::OK);

    // 验证会话已被标记为 admin_ended = true
    let s = store.get_session(user_id, device_id).unwrap().unwrap();
    assert!(s.admin_ended);

    // 3. 客户端发送当前会话的普通 progress 心跳，触发服务端收尾并关闭会话
    let progress_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/progress",
            Some("management-secret"),
            json!({
                "media_item_id": media.id.to_string(),
                "file_id": file_id.to_string(),
                "device_id": device_id,
                "event": "progress",
                "play_session_id": session,
                "event_sequence": next_playback_sequence(),
                "position_ms": 2000,
                "duration_ms": 100000
            }),
        ))
        .await
        .unwrap();
    assert_eq!(progress_res.status(), StatusCode::OK);
    let progress_body = json_data(progress_res).await;
    assert_eq!(progress_body["ended_by_admin"], true);

    // 验证会话此时已彻底从 playback_sessions 表中关闭
    assert!(store.get_session(user_id, device_id).unwrap().is_none());

    // 4. 客户端再次发来 progress 心跳，验证系统绝不自动“复活”新会话，且仍返回 ended_by_admin = true (G03)
    let second_progress_res = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/playback/progress",
            Some("management-secret"),
            json!({
                "media_item_id": media.id.to_string(),
                "file_id": file_id.to_string(),
                "device_id": device_id,
                "event": "progress",
                "play_session_id": session,
                "event_sequence": next_playback_sequence(),
                "position_ms": 3000,
                "duration_ms": 100000
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_progress_res.status(), StatusCode::OK);
    let second_body = json_data(second_progress_res).await;
    assert_eq!(
        second_body["ended_by_admin"], true,
        "已被管理员结束的会话在后续心跳中必须告知客户端已结束"
    );

    assert!(
        store.get_session(user_id, device_id).unwrap().is_none(),
        "已被管理员结束的会话绝不能在后续 progress 心跳中自动复活！(G03)"
    );
}

#[tokio::test]
async fn borrowed_marker_clears_outro_and_does_not_cache_permanently() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "AnimeSeries".into(),
            year: Some(2024),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("22222".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let ep1_file = tmp.path().join("data/library/tv/AnimeSeries/S01E01.mkv");
    let ep2_file = tmp.path().join("data/library/tv/AnimeSeries/S01E02.mkv");
    std::fs::create_dir_all(ep1_file.parent().unwrap()).unwrap();
    std::fs::write(&ep1_file, b"ep1").unwrap();
    std::fs::write(&ep2_file, b"ep2").unwrap();

    let ep1_id = LedgerId::new();
    let ep2_id = LedgerId::new();
    for (file_id, path, episode) in [
        (ep1_id, ep1_file.display().to_string(), 1u32),
        (ep2_id, ep2_file.display().to_string(), 2u32),
    ] {
        publish_fixture_file(
            &store,
            &FixtureFile {
                file_id: Some(file_id),
                media_id: media.id,
                path,
                season: Some(1),
                episode: Some(episode),
                resolution: Some("1080p".into()),
                ..Default::default()
            },
        )
        .unwrap();
    }

    // 在 Episode 1 写入完整的片头和片尾标记
    store
        .put_media_marker(&store::StoredMediaMarker {
            media_id: media.id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(10_000),
            intro_end_ms: Some(100_000),
            outro_start_ms: Some(1_300_000),
            outro_end_ms: Some(1_400_000),
            source: "theintrodb".into(),
            locked: false,
            updated_at: 1000,
        })
        .unwrap();
    // Episode 2 写入仅包含空 intro_start_ms 的初始缓存 (is_only_intro 状态)
    store
        .put_cached_chapters(
            &ep2_id.to_string(),
            &[marker::ChapterMarker {
                start_ms: 0,
                end_ms: 10_000,
                title: Some("Intro".into()),
                marker_type: Some(marker::MarkerType::IntroStart),
                synthetic: false,
            }],
        )
        .unwrap();
    let coverage = store.file_coverage_by_id(ep2_id).unwrap().unwrap();
    let session = store
        .issue_play_session(
            store.admin_user_id().unwrap(),
            "chapter-test",
            ep2_id,
            media.id,
            domain::UnitKey::new(1, 2),
            &coverage.file.version,
            0,
        )
        .unwrap();
    drop(store);

    // 请求 Episode 2 的章节信息
    let chapters_res = app
        .clone()
        .oneshot(request(
            "GET",
            &format!("/api/v1/playback/chapters?play_session_id={session}&device_id=chapter-test"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(chapters_res.status(), StatusCode::OK);
    let chapters_body = json_data(chapters_res).await;
    let chapters = chapters_body["chapters"].as_array().unwrap();

    // 验证：借用的章节必须只包含片头 (intro)，绝对不借用 ep1 的片尾 (outro) (G08)
    let has_outro = chapters.iter().any(|c| {
        c["title"].as_str().unwrap_or_default().contains("Outro")
            || c["title"].as_str().unwrap_or_default().contains("片尾")
            || c["start_ms"] == 1_300_000
    });
    assert!(
        !has_outro,
        "借用其他集的 marker 绝对不能借用片尾 outro 绝对时间！(G08)"
    );

    // 验证：借用生成的内容绝不被永久写入数据库 put_cached_chapters，保留自愈能力 (G08)
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let ep2_persisted = store
        .get_cached_chapters(&ep2_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(ep2_persisted.len(), 1, "借用的暂态章节标记不应永久覆盖缓存");
}

fn seed_movie(tmp: &tempfile::TempDir) -> (api::Store, Media, LedgerId) {
    let store = api::Store::open(tmp.path().join("data")).unwrap();
    let media = store
        .ensure_media(Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "TestMovie".into(),
            year: Some(2023),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: Some("11111".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();

    let movie_file = tmp.path().join("data/library/movies/TestMovie.mkv");
    std::fs::create_dir_all(movie_file.parent().unwrap()).unwrap();
    std::fs::write(&movie_file, b"video").unwrap();

    let file_id = LedgerId::new();
    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(file_id),
            media_id: media.id,
            path: movie_file.display().to_string(),
            resolution: Some("1080p".into()),
            ..Default::default()
        },
    )
    .unwrap();
    (store, media, file_id)
}

use api::{Store, router};
use axum::http::StatusCode;
use downloader::{Downloader, DownloaderError, MemoryDownloader};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tower::ServiceExt;

use super::common::*;
use super::subscription_depth::{authed_app, seed_pending_torrent};
use store::test_support::{FixtureFile, publish_fixture_file};

async fn setup_tv_subscription_with_two_seasons(
    app: &axum::Router,
    tmp: &tempfile::TempDir,
) -> (String, String) {
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" },
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();
    let media_id = created["data"]["media"]["id"].as_str().unwrap().to_string();

    let store = Store::open(tmp.path().join("data")).unwrap();
    let dir = tmp.path().join("data/library/tv");
    std::fs::create_dir_all(&dir).unwrap();
    for (season, episode) in [(1u32, 1u32), (2, 1)] {
        let path = dir.join(format!("s{season}e{episode}.mkv"));
        std::fs::write(&path, b"ep").unwrap();
        publish_fixture_file(
            &store,
            &FixtureFile {
                file_id: Some(domain::LedgerId::new()),
                media_id: media_id.parse().unwrap(),
                path: path.display().to_string(),
                season: Some(season),
                episode: Some(episode),
                resolution: Some("1080p".into()),
                codec: Some("h264".into()),
                hdr: None,
                quality_source: domain::QualitySource::Probe,
                confidence: domain::Confidence::High,
                filter_score: Some(80),
                ..Default::default()
            },
        )
        .unwrap();
    }
    (id, media_id)
}

#[tokio::test]
async fn season_cleanup_bins_out_of_scope_files() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let (id, media_id) = setup_tv_subscription_with_two_seasons(&app, &tmp).await;

    let cleanup = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/subscriptions/{id}/season-cleanup"),
                Some("management-secret"),
                json!({ "seasons": [2], "delete_library_files": true, "delete_torrents": false }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(cleanup["data"]["binned"], 1);

    let store = Store::open(tmp.path().join("data")).unwrap();
    let remaining: Vec<_> = store
        .list_file_coverage()
        .unwrap()
        .into_iter()
        .filter(|coverage| coverage.file.media_id == media_id.parse().unwrap())
        .collect();
    assert_eq!(remaining.len(), 1, "季 2 文件应移出 ledger");
    assert_eq!(remaining[0].units, vec![domain::UnitKey::new(1, 1)]);
}

#[tokio::test]
async fn season_cleanup_keeps_ledger_when_physical_delete_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" },
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap();
    let media_id = created["data"]["media"]["id"].as_str().unwrap();
    let blocked = tmp.path().join("data/library/tv/blocked.mkv");
    std::fs::create_dir_all(&blocked).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(domain::LedgerId::new()),
            media_id: media_id.parse().unwrap(),
            path: blocked.display().to_string(),
            season: Some(2),
            episode: Some(1),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap();
    drop(store);
    let cleanup = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{id}/season-cleanup"),
            Some("management-secret"),
            json!({ "seasons": [2], "delete_library_files": true }),
        ))
        .await
        .unwrap();
    assert_eq!(cleanup.status(), StatusCode::CONFLICT);
    let cleanup = json_body(cleanup).await;
    assert_eq!(cleanup["error"]["code"], "subscription.file_cleanup_failed");
    let remaining = Store::open(tmp.path().join("data"))
        .unwrap()
        .file_coverage_for_media(media_id.parse().unwrap())
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert!(blocked.is_dir());
}

#[tokio::test]
async fn season_cleanup_removes_torrents_from_client() {
    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader.clone(),
    ));
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" },
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();
    seed_pending_torrent(
        &tmp,
        &id,
        "The.Long.Watch.2024.S02E01.1080p.WEB-DL",
        "https://pt.example/dl/2",
    );

    let cleanup = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/subscriptions/{id}/season-cleanup"),
                Some("management-secret"),
                json!({ "seasons": [2], "delete_torrents": true, "delete_library_files": false }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(cleanup["data"]["pending_cleared"], 1);
    assert_eq!(
        cleanup["data"]["removed_from_client"], 1,
        "季清理应同步删下载器种子"
    );
    assert_eq!(
        downloader.removed().len(),
        1,
        "MemoryDownloader 应记录被删种子"
    );
}

#[derive(Default)]
struct PartialOwnedDownloader {
    removed: Mutex<Vec<String>>,
}

impl Downloader for PartialOwnedDownloader {
    fn add(&self, _: &domain::Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }

    fn completed_files(
        &self,
        _: &domain::Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        Ok(Vec::new())
    }

    fn owned_identity(&self, torrent: &domain::Torrent) -> Result<Option<String>, DownloaderError> {
        Ok(downloader::magnet_info_hash(&torrent.enclosure))
    }

    fn remove_owned(&self, torrent: &domain::Torrent, _: bool) -> Result<(), DownloaderError> {
        if torrent.enclosure.contains("keep") {
            return Err(DownloaderError::Message("keep this task".into()));
        }
        if self.owned_identity(torrent)?.is_none() {
            return Err(DownloaderError::Message(
                "owned identity unproven: HTTP enclosure could not prove an actual downloader identity; refusing unsafe cleanup"
                    .into(),
            ));
        }
        self.removed.lock().push(torrent.enclosure.clone());
        Ok(())
    }

    fn delete_task(&self, info_hash: &str, _: bool) -> Result<(), DownloaderError> {
        if info_hash.bytes().all(|b| b == b'f') {
            return Err(DownloaderError::Message("keep this task".into()));
        }
        self.removed.lock().push(info_hash.to_string());
        Ok(())
    }
}

#[tokio::test]
async fn season_cleanup_persists_successful_removals_before_later_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(PartialOwnedDownloader::default());
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader.clone(),
    ));
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" },
                    "coverage": { "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();
    seed_pending_torrent(
        &tmp,
        &id,
        "The.Long.Watch.2024.S02E01.1080p.WEB-DL",
        "magnet:?xt=urn:btih:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
    );
    seed_pending_torrent(
        &tmp,
        &id,
        "The.Long.Watch.2024.S02E02.1080p.WEB-DL",
        "magnet:?xt=urn:btih:ffffffffffffffffffffffffffffffffffffffff&keep=1",
    );
    let cleanup = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{id}/season-cleanup"),
            Some("management-secret"),
            json!({ "seasons": [2], "delete_torrents": true, "delete_library_files": false }),
        ))
        .await
        .unwrap();
    assert_eq!(cleanup.status(), StatusCode::BAD_GATEWAY);
    let store = Store::open(tmp.path().join("data")).unwrap();
    let remaining = store
        .load_pending(id.parse().unwrap())
        .unwrap()
        .into_iter()
        .map(|(_, pending)| pending.torrent.enclosure)
        .collect::<Vec<_>>();
    assert_eq!(remaining.len(), 1);
    assert!(remaining[0].contains("keep"));
    assert_eq!(downloader.removed.lock().len(), 1);
}

async fn create_sub_and_seed_torrent(app: &axum::Router, tmp: &tempfile::TempDir) -> String {
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "movie", "title": "The Matrix" },
                    "coverage": { "kind": "movie" },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();
    seed_pending_torrent(
        tmp,
        &id,
        "The.Matrix.1999.2160p.BluRay.x265-GROUP",
        "https://pt.example/dl/1",
    );
    id
}

#[tokio::test]
async fn delete_subscription_can_cascade_torrent_removal() {
    let tmp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloader.clone(),
    ));
    let id = create_sub_and_seed_torrent(&app, &tmp).await;

    let deleted = json_body(
        app.clone()
            .oneshot(request(
                "DELETE",
                &format!("/api/v1/subscriptions/{id}?delete_torrents=true"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(deleted["data"]["deleted"], true);
    assert_eq!(deleted["data"]["removed_from_client"], 1);
    assert_eq!(downloader.removed().len(), 1);

    // 订阅已不存在。
    let gone = app
        .oneshot(request(
            "GET",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn season_cleanup_deletes_the_whole_file_when_selected_units_share_it() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let (id, media_id) = setup_tv_subscription_with_two_seasons(&app, &tmp).await;
    let store = Store::open(tmp.path().join("data")).unwrap();
    let directory = tmp.path().join("data/library/tv");
    let shared = directory.join("shared-s1-s2.mkv");
    std::fs::write(&shared, b"shared").unwrap();
    store::test_support::publish_fixture_file_units(
        &store,
        &FixtureFile {
            media_id: media_id.parse().unwrap(),
            path: shared.display().to_string(),
            ..Default::default()
        },
        &[domain::UnitKey::new(1, 2), domain::UnitKey::new(2, 2)],
    )
    .unwrap();
    let result = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{id}/season-cleanup"),
            Some("management-secret"),
            json!({
                "seasons": [2], "delete_library_files": true, "delete_torrents": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(result.status(), StatusCode::OK);
    assert_eq!(json_body(result).await["data"]["binned"], 2);
    assert!(
        !shared.exists(),
        "a physical file cannot retain its unselected-season bytes"
    );
    assert!(!directory.join("s2e1.mkv").exists());
    assert!(directory.join("s1e1.mkv").exists());
    let remaining = store
        .file_coverage_for_media(media_id.parse().unwrap())
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].units, vec![domain::UnitKey::new(1, 1)]);
}

#[tokio::test]
async fn season_cleanup_skips_files_outside_library_root_and_owned_by_other_subscribe() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let (id, media_id) = setup_tv_subscription_with_two_seasons(&app, &tmp).await;
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media_uuid: domain::MediaId = media_id.parse().unwrap();

    // Seed Subscribe B owning S2E2 on the same media.
    let sub_b = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" },
                    "coverage": { "kind": "tv", "season": 2, "episode_from": 2, "episode_to": 2 },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(sub_b["ok"], true);

    let tv_dir = tmp.path().join("data/library/tv");
    let s2e2 = tv_dir.join("s2e2.mkv");
    std::fs::write(&s2e2, b"sub-b").unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media_uuid,
            path: s2e2.display().to_string(),
            season: Some(2),
            episode: Some(2),
            ..Default::default()
        },
    )
    .unwrap();

    let outside_dir = tmp.path().join("outside");
    std::fs::create_dir_all(&outside_dir).unwrap();
    let outside_seed = tv_dir.join("outside-seed.mkv");
    std::fs::write(&outside_seed, b"outside").unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media_uuid,
            path: outside_seed.display().to_string(),
            season: Some(3),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    let outside = outside_dir.join("outside.mkv");
    std::fs::rename(&outside_seed, &outside).unwrap();
    assert!(
        store::test_support::force_file_path(
            &store,
            &outside_seed.display().to_string(),
            &outside.display().to_string(),
        )
        .unwrap()
    );

    let s4e1 = tv_dir.join("s4e1.mkv");
    std::fs::write(&s4e1, b"s4").unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media_uuid,
            path: s4e1.display().to_string(),
            season: Some(4),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();

    let resp = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{id}/season-cleanup"),
            Some("management-secret"),
            json!({ "seasons": [2, 3, 4], "delete_library_files": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let data = json_body(resp).await["data"].clone();
    assert_eq!(
        data["binned"], 2,
        "only s2e1.mkv and s4e1.mkv should be binned: {data}"
    );
    assert!(s2e2.exists(), "file owned by Subscribe B must stay");
    assert!(outside.exists(), "file outside library root must stay");
    assert!(
        !tv_dir.join("s2e1.mkv").exists(),
        "season 2 unowned file must be binned"
    );
    assert!(!s4e1.exists(), "unowned file inside root must be binned");
}

#[tokio::test]
async fn season_cleanup_clears_subscribe_facts_for_deleted_and_shared_files_only() {
    let tmp = tempfile::tempdir().unwrap();
    let app = authed_app(&tmp);
    let (id, media_id) = setup_tv_subscription_with_two_seasons(&app, &tmp).await;
    let store = Store::open(tmp.path().join("data")).unwrap();
    let sid: domain::SubscribeId = id.parse().unwrap();
    let dir = tmp.path().join("data/library/tv");
    let shared = dir.join("shared-s1-s2.mkv");
    std::fs::write(&shared, b"shared").unwrap();
    store::test_support::publish_fixture_file_units(
        &store,
        &FixtureFile {
            media_id: media_id.parse().unwrap(),
            path: shared.display().to_string(),
            ..Default::default()
        },
        &[domain::UnitKey::new(1, 2), domain::UnitKey::new(2, 2)],
    )
    .unwrap();

    let mut facts = domain::SubscribeFacts::default();
    let s1e1_path = dir.join("s1e1.mkv").display().to_string();
    let shared_path = shared.display().to_string();
    let s2e1_path = dir.join("s2e1.mkv").display().to_string();
    facts.upsert(
        Some(1),
        Some(1),
        domain::QualityFact {
            score: 80,
            path: Some(s1e1_path.clone()),
        },
    );
    facts.upsert(
        Some(1),
        Some(2),
        domain::QualityFact {
            score: 80,
            path: Some(shared_path.clone()),
        },
    );
    facts.upsert(
        Some(2),
        Some(1),
        domain::QualityFact {
            score: 80,
            path: Some(s2e1_path),
        },
    );
    facts.upsert(
        Some(2),
        Some(2),
        domain::QualityFact {
            score: 80,
            path: Some(shared_path),
        },
    );
    store.save_subscribe_facts(sid, &facts).unwrap();

    // No-op cleanup (delete_library_files=false) must preserve all subscribe_facts.
    let noop = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{id}/season-cleanup"),
            Some("management-secret"),
            json!({ "seasons": [2], "delete_library_files": false, "delete_torrents": false }),
        ))
        .await
        .unwrap();
    assert_eq!(noop.status(), StatusCode::OK);
    assert_eq!(
        store.load_subscribe_facts(sid).unwrap().entries().count(),
        4
    );

    // Physical cleanup of season 2 deletes s2e1 + shared, clearing (2,1), (2,2), and (1,2), keeping (1,1).
    let resp = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{id}/season-cleanup"),
            Some("management-secret"),
            json!({ "seasons": [2], "delete_library_files": true, "delete_torrents": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let remaining = store.load_subscribe_facts(sid).unwrap();
    assert_eq!(remaining.entries().count(), 1);
    assert_eq!(
        remaining
            .get(Some(1), Some(1))
            .and_then(|f| f.path.as_deref()),
        Some(s1e1_path.as_str())
    );
    let detail = json_body(
        app.oneshot(request(
            "GET",
            &format!("/api/v1/subscriptions/{id}"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(detail["data"]["progress"]["imported"], 1);
}

struct BlockingCompletedDownloader {
    stage: std::path::PathBuf,
    started_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<()>>>,
    resume_rx: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}
impl Downloader for BlockingCompletedDownloader {
    fn add(&self, _: &domain::Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }
    fn completed_files(
        &self,
        _: &domain::Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        if let Some(tx) = self.started_tx.lock().take() {
            let _ = tx.send(());
        }
        if let Some(rx) = self.resume_rx.lock().take() {
            let _ = rx.blocking_recv();
        }
        Ok(vec![self.stage.join("The.Matrix.1999.1080p.mkv")])
    }
}

#[tokio::test]
async fn season_cleanup_rejects_deleting_subscribe_and_waits_for_active_transfer() {
    let tmp = tempfile::tempdir().unwrap();
    let stage = tmp.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join("The.Matrix.1999.1080p.mkv"), b"vid").unwrap();
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    let api_state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(BlockingCompletedDownloader {
            stage,
            started_tx: Mutex::new(Some(started_tx)),
            resume_rx: Mutex::new(Some(resume_rx)),
        }),
    );
    let app = router(api_state.clone());
    let created = json_body(
        app.clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some("management-secret"),
                json!({
                    "media": { "kind": "movie", "title": "The Matrix", "year": 1999 },
                    "coverage": { "kind": "movie" },
                }),
            ))
            .await
            .unwrap(),
    )
    .await;
    let id = created["data"]["id"].as_str().unwrap().to_string();
    let sid: domain::SubscribeId = id.parse().unwrap();

    // 1. Mark deleting -> 409 subscription.deleting
    api_state.mark_subscribe_deleting(sid);
    let deleting_resp = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{id}/season-cleanup"),
            Some("management-secret"),
            json!({ "seasons": [1], "delete_library_files": true }),
        ))
        .await
        .unwrap();
    assert_eq!(deleting_resp.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_error(deleting_resp).await["code"],
        "subscription.deleting"
    );
    api_state.clear_subscribe_deleting(sid);

    // 2. Active transfer in flight -> season-cleanup waits until transfer finishes
    seed_pending_torrent(
        &tmp,
        &id,
        "The.Matrix.1999.1080p",
        "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    let transfer_state = api_state.clone();
    let transfer =
        tokio::task::spawn_blocking(move || api::worker::transfer_one(&transfer_state, sid));
    tokio::time::timeout(std::time::Duration::from_secs(5), started_rx.recv())
        .await
        .expect("transfer must enter completed_files")
        .unwrap();
    let cleanup_app = app.clone();
    let cleanup_uri = format!("/api/v1/subscriptions/{id}/season-cleanup");
    let mut cleanup = tokio::spawn(async move {
        cleanup_app
            .oneshot(request(
                "POST",
                &cleanup_uri,
                Some("management-secret"),
                json!({ "seasons": [1], "delete_library_files": true }),
            ))
            .await
            .unwrap()
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(120), &mut cleanup)
            .await
            .is_err(),
        "season-cleanup must wait for active transfer to finish"
    );
    resume_tx.send(()).unwrap();
    transfer.await.unwrap().unwrap();
    let resp = tokio::time::timeout(std::time::Duration::from_secs(5), cleanup)
        .await
        .expect("season-cleanup must resume after active transfer finishes")
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

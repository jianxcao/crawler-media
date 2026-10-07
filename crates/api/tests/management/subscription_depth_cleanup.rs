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
    let dir = tmp.path().join("tv");
    std::fs::create_dir_all(&dir).unwrap();
    for (season, episode) in [(1u32, 1u32), (2, 1)] {
        let path = dir.join(format!("s{season}e{episode}.mkv"));
        std::fs::write(&path, b"ep").unwrap();
        store
            .insert_ledger(&domain::LedgerRow {
                id: domain::LedgerId::new(),
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
            })
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
        .list_ledger()
        .unwrap()
        .into_iter()
        .filter(|row| row.media_id == media_id.parse().unwrap())
        .collect();
    assert_eq!(remaining.len(), 1, "季 2 文件应移出 ledger");
    assert_eq!(remaining[0].season, Some(1));
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
    let blocked = tmp.path().join("blocked.mkv");
    std::fs::create_dir_all(&blocked).unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .insert_ledger(&domain::LedgerRow {
            id: domain::LedgerId::new(),
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
        })
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
        .ledger_for_media(media_id.parse().unwrap())
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

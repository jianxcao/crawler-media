//! Tests proving retry of transfer completes pending tasks when subtitles are restored.

use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use axum::http::StatusCode;
use domain::Torrent;
use downloader::{Downloader, DownloaderError};
use parking_lot::Mutex;
use serde_json::json;
use tower::ServiceExt;

use super::common::{Fixtures, json_data, request, state, subscribe_payload};

#[tokio::test]
async fn disabling_nfo_mirroring_does_not_disable_transfer_artwork() {
    let tmp = tempfile::tempdir().unwrap();
    let stage = tmp.path().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join("The.Matrix.1999.1080p.mkv"), b"video").unwrap();
    std::fs::write(stage.join("The.Matrix.1999.1080p.zh.srt"), b"subtitle").unwrap();
    let api_state = state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(SubtitleRetryDownloader { stage_dir: stage }),
    )
    .with_catalog(Arc::new(super::catalog::PosterCatalog))
    .with_poster_fetch(Arc::new(super::unidentified::StaticPoster));
    api_state.store().lock().set_scrape_enabled(true).unwrap();
    let app = router(api_state.clone());
    let response = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/settings/scrape",
            Some("management-secret"),
            json!({"setting": {"mirror_nfo": false, "mirror_images": true}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let libs = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/libraries?kind=movie",
                Some("management-secret"),
                serde_json::Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let lib = libs[0]["id"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(request(
            "PATCH",
            &format!("/api/v1/libraries/{lib}"),
            Some("management-secret"),
            json!({"scrape": true}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = subscribe_payload("search");
    body["media"] = json!({"kind": "movie", "title": "The Matrix", "year": 1999, "tmdb_id": "603"});
    let created = json_data(
        app.oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            body,
        ))
        .await
        .unwrap(),
    )
    .await;
    let id = created["id"].as_str().unwrap().parse().unwrap();
    let torrent = Torrent {
        id: None,
        site_id: domain::SiteId::new(),
        title: "The.Matrix.1999.1080p".into(),
        enclosure: "magnet:?xt=urn:btih:matrix-artwork".into(),
        size_bytes: None,
        seeders: None,
        free: false,
        hr: false,
        imdb_id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    };
    api_state
        .store()
        .lock()
        .record_pending_submission(
            id,
            100,
            &api::store::PendingDownload {
                submitted_at: Some(1000),
                torrent,
                release_override: None,
                downloader_id: None,
            },
        )
        .unwrap();
    let runner = api_state.clone();
    tokio::task::spawn_blocking(move || api::worker::transfer_one(&runner, id))
        .await
        .unwrap()
        .unwrap();
    let rows = api_state.store().lock().list_ledger().unwrap();
    assert_eq!(rows.len(), 1);
    let video = std::path::Path::new(&rows[0].path);
    assert!(!video.with_extension("nfo").exists());
    assert_eq!(
        std::fs::read(video.parent().unwrap().join("poster.jpg")).unwrap(),
        b"poster-bytes"
    );
}

struct SubtitleRetryDownloader {
    stage_dir: std::path::PathBuf,
}

impl Downloader for SubtitleRetryDownloader {
    fn add(&self, _torrent: &Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }

    fn remove(&self, _torrent: &Torrent, _delete_files: bool) -> Result<(), DownloaderError> {
        Ok(())
    }

    fn completed_files(
        &self,
        _torrent: &Torrent,
    ) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        Ok(vec![
            self.stage_dir.join("The.Matrix.1999.1080p.mkv"),
            self.stage_dir.join("The.Matrix.1999.1080p.zh.srt"),
        ])
    }
}

mod fixture;
use fixture::{TransferFixture, torrent};
mod scenarios;

#[tokio::test]
async fn transfer_retry_completes_pending_after_subtitle_failure_is_resolved() {
    let fixture = TransferFixture::new(
        false,
        "{title} ({year})/{title} ({year}) - {resolution}{ext}",
    )
    .await;
    let torrent = torrent("The.Matrix.1999.1080p", "matrix");
    let video = fixture.source("The.Matrix.1999.1080p.mkv", b"video");
    let subtitle = fixture.source("The.Matrix.1999.1080p.zh.srt", b"subtitle");
    fixture.files(&torrent, &[video.clone(), subtitle]);
    fixture.pending(&torrent);
    let destination = fixture.destination("movies/The Matrix (1999)/The Matrix (1999) - 1080p.mkv");
    let blocker = destination.with_extension("zh.srt");
    std::fs::create_dir_all(&blocker).unwrap();
    assert!(
        fixture.transfer().await.is_err(),
        "Round 1 transfer should fail because subtitle was blocked"
    );
    assert_eq!(
        fixture
            .state
            .store()
            .lock()
            .ledger_for_media(fixture.media_id)
            .unwrap()
            .len(),
        1,
        "Video must remain in ledger"
    );
    fixture.assert_pending(1, 0);
    fixture.unblock(
        &blocker,
        "movies/The Matrix (1999)/The Matrix (1999) - 1080p.zh.srt",
    );
    let result = fixture.transfer().await;
    assert!(
        result.is_ok(),
        "Round 2 transfer should succeed: {result:?}"
    );
    assert_eq!(
        fixture
            .state
            .store()
            .lock()
            .ledger_for_media(fixture.media_id)
            .unwrap()
            .len(),
        1,
        "No duplicate video ledger"
    );
    fixture.assert_pending(0, 1);
    fixture.assert_sources(&[(&video, &destination)]);
    assert_eq!(std::fs::read(blocker).unwrap(), b"subtitle");
}

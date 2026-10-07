use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use api::{Store, router};
use axum::http::StatusCode;
use downloader::{Downloader, DownloaderError};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

struct FailingDownloader {
    good_file: PathBuf,
}

impl Downloader for FailingDownloader {
    fn add(&self, _: &domain::Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }

    fn completed_files(&self, torrent: &domain::Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        if torrent.enclosure.ends_with("/bad") {
            Err(DownloaderError::Message("fake Downloader failure".into()))
        } else {
            Ok(vec![self.good_file.clone()])
        }
    }
}

fn torrent(title: &str, enclosure: &str) -> domain::Torrent {
    domain::Torrent {
        site_id: domain::SiteId::new(),
        title: title.into(),
        enclosure: enclosure.into(),
        size_bytes: None,
        seeders: None,
        free: false,
        hr: false,
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    }
}

#[tokio::test]
async fn transfer_continues_to_later_subscribe_and_keeps_job_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let good_file = tmp.path().join("Dune.2024.1080p.mkv");
    std::fs::write(&good_file, b"fixture").unwrap();
    let app = router(state_with_downloader(tmp.path(), good_file));
    let first = create_subscribe(&app, "search").await;
    let mut payload = subscribe_payload("search");
    payload["media"] = json!({"kind":"movie", "title":"Dune"});
    let second_response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            payload,
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::CREATED);
    let second = json_data(second_response).await;
    let store = Store::open(tmp.path().join("data")).unwrap();
    let first_id = first["id"].as_str().unwrap().parse().unwrap();
    let second_id = second["id"].as_str().unwrap().parse().unwrap();
    for (id, title, enclosure) in [
        (first_id, "The.Matrix.1999.1080p", "https://pt.example/bad"),
        (second_id, "Dune.2024.1080p", "https://pt.example/good"),
    ] {
        store
            .merge_pending(
                id,
                &[(
                    80,
                    api::store::PendingDownload {
                        torrent: torrent(title, enclosure),
                        release_override: None,
                        downloader_id: None,
                        submitted_at: None,
                    },
                )],
            )
            .unwrap();
    }
    let queue = jobs::Queue::open(tmp.path().join("data/jobs.db")).unwrap();
    let transfer_def = queue
        .list_defs()
        .unwrap()
        .into_iter()
        .find(|row| row.kind == jobs::JobKind::Transfer)
        .unwrap();
    let mut failed = false;
    for now in [1, 31, 61, 91, 121, 151, 181, 211, 241, 271] {
        let tick = app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/jobs/tick?now={now}"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(tick.status(), StatusCode::OK);
        failed |= queue
            .get_live_child(transfer_def.id)
            .unwrap()
            .is_some_and(|job| job.error.as_deref() == Some("1 Subscribe Transfer failed"));
        if failed && store.list_ledger().unwrap().len() == 1 {
            break;
        }
    }
    assert!(failed, "Transfer Job should report the per-Subscribe error");
    let ledger = store.list_ledger().unwrap();
    assert_eq!(ledger.len(), 1);
    assert_eq!(
        ledger[0].media_id,
        second["media"]["id"].as_str().unwrap().parse().unwrap()
    );
    assert_eq!(
        store
            .load_pending_state(second_id, "imported")
            .unwrap()
            .len(),
        1
    );
}

fn state_with_downloader(root: &std::path::Path, good_file: PathBuf) -> api::ApiState {
    api::ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(FailingDownloader { good_file }),
        root.join("library"),
    )
    .unwrap()
}

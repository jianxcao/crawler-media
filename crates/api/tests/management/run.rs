use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::Torrent;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

struct OneSiteFails {
    failed_site_id: Mutex<Option<String>>,
}

impl Fetcher for OneSiteFails {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        let site_id = request.key.split(':').nth(1).unwrap_or_default();
        if self.failed_site_id.lock().as_deref() == Some(site_id) {
            return Err(IndexerError::Fetch("fixture site failure".into()));
        }
        if request.key.starts_with("search:") {
            Ok(nexusphp().into())
        } else {
            Err(IndexerError::Fetch("unexpected fixture request".into()))
        }
    }
}

#[tokio::test]
async fn subscribe_run_persists_transferred_library_ledger_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let source = tmp.path().join("matrix.mkv");
    std::fs::write(
        &source,
        r#"{"resolution":"2160p","codec":"hevc","hdr":"hdr10"}"#,
    )
    .unwrap();
    downloader.map_enclosure("https://pt.example/download.php?id=1&passkey=abc", source);
    let app = router(state(tmp.path(), fetcher, downloader));
    create_site(&app).await;
    let subscribe = create_subscribe(&app, "search").await;

    let run_response = app
        .clone()
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/subscriptions/{}/run",
                subscribe["id"].as_str().unwrap()
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(run_response.status(), StatusCode::OK);
    let run = json_data(run_response).await;
    assert_eq!(run["ledger_rows"], 1);
    assert_eq!(run["completed"], true);

    let response = app
        .oneshot(request(
            "GET",
            "/api/v1/ledger",
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let rows = json_data(response).await;
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["media_title"], "The Matrix");
    assert_eq!(rows[0]["resolution"], "2160p");
    assert!(Path::new(rows[0]["path"].as_str().unwrap()).is_file());
    let stored = api::Store::open(tmp.path().join("data")).unwrap();
    let id = subscribe["id"].as_str().unwrap().parse().unwrap();
    assert!(stored.load_pending(id).unwrap().is_empty());
    assert_eq!(stored.load_pending_state(id, "imported").unwrap().len(), 1);
}

#[allow(dead_code)]
fn _torrent_type_anchor(torrent: Torrent) -> Torrent {
    torrent
}

#[tokio::test]
async fn persisted_facts_prevent_redownload_after_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let source = tmp.path().join("matrix.mkv");
    std::fs::write(&source, b"fixture").unwrap();
    downloader.map_enclosure("https://pt.example/download.php?id=1&passkey=abc", source);

    let app = router(state(tmp.path(), fetcher.clone(), downloader.clone()));
    create_site(&app).await;
    let subscribe = create_subscribe(&app, "search").await;
    let run_uri = format!(
        "/api/v1/subscriptions/{}/run",
        subscribe["id"].as_str().unwrap()
    );
    let first = app
        .clone()
        .oneshot(request(
            "POST",
            &run_uri,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(downloader.added().len(), 1);
    drop(app);

    let restarted = router(state(tmp.path(), fetcher, downloader.clone()));
    let second = restarted
        .oneshot(request(
            "POST",
            &run_uri,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();

    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(json_data(second).await["ledger_rows"], 0);
    assert_eq!(downloader.added().len(), 1);
}

#[tokio::test]
async fn legacy_run_does_not_dispatch_an_active_pending_torrent_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::from([("search", nexusphp())]),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(state(tmp.path(), fetcher, downloader.clone()));
    create_site(&app).await;
    let subscribe = create_subscribe(&app, "search").await;
    let subscribe_id = subscribe["id"].as_str().unwrap();
    let run_uri = format!("/api/v1/subscriptions/{subscribe_id}/run");

    let first = app
        .clone()
        .oneshot(request(
            "POST",
            &run_uri,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(downloader.added().len(), 1);
    let stored = api::Store::open(tmp.path().join("data")).unwrap();
    let subscribe_id = subscribe_id.parse().unwrap();
    assert_eq!(stored.load_pending(subscribe_id).unwrap().len(), 1);

    let second = app
        .oneshot(request(
            "POST",
            &run_uri,
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(downloader.added().len(), 1);
}

#[tokio::test]
async fn legacy_run_reports_failure_when_every_site_search_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let app = router(state(
        tmp.path(),
        fetcher,
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    ));
    create_site(&app).await;
    let subscribe = create_subscribe(&app, "search").await;

    let response = app
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/subscriptions/{}/run",
                subscribe["id"].as_str().unwrap()
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let error = json_error(response).await;
    assert_eq!(error["code"], "subscribe.run_failed");
    assert!(
        error["message"].as_str().unwrap().contains("failed")
            || error["message"].as_str().unwrap().contains("失败")
    );
}

#[tokio::test]
async fn legacy_run_keeps_results_when_one_site_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(OneSiteFails {
        failed_site_id: Mutex::new(None),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let state = ApiState::new(
        Store::open(tmp.path().join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        fetcher.clone(),
        downloader.clone(),
        tmp.path().join("library"),
    )
    .unwrap();
    let app = router(state);
    create_site(&app).await;
    let mut failing_site = site_payload();
    failing_site["name"] = "failing".into();
    let response = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/sites",
            Some("management-secret"),
            failing_site,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let failing_site = json_data(response).await;
    *fetcher.failed_site_id.lock() = Some(failing_site["id"].as_str().unwrap().into());
    let subscribe = create_subscribe(&app, "search").await;

    let response = app
        .oneshot(request(
            "POST",
            &format!(
                "/api/v1/subscriptions/{}/run",
                subscribe["id"].as_str().unwrap()
            ),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(downloader.added().len(), 1);
}

#[tokio::test]
async fn pending_torrent_occupies_slot_and_prevents_duplicate_download_in_next_round() {
    let tmp = tempfile::tempdir().unwrap();
    let round = Arc::new(Mutex::new(0));
    let round_fetcher = round.clone();
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));

    struct MultiRoundFetcher(Arc<Mutex<usize>>);
    impl Fetcher for MultiRoundFetcher {
        fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
            let mut r = self.0.lock();
            *r += 1;
            if *r == 1 {
                // 第一轮：发现种子 A (id=1)
                Ok(nexusphp().into())
            } else {
                // 第二轮：发现同剧集不同下载地址的种子 B (id=2)
                Ok(nexusphp().replace("download.php?id=1", "download.php?id=2"))
            }
        }
    }

    let app = router(
        ApiState::new(
            Store::open(tmp.path().join("data")).unwrap(),
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(MultiRoundFetcher(round_fetcher)),
            downloader.clone(),
            tmp.path().join("library"),
        )
        .unwrap(),
    );
    create_site(&app).await;
    let subscribe = create_subscribe(&app, "search").await;
    let sub_id = subscribe["id"].as_str().unwrap();

    // 第一轮搜索并添加，种子在途 (pending)
    let run1 = app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{sub_id}/run"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(run1.status(), StatusCode::OK);
    assert_eq!(downloader.added().len(), 1, "第一轮添加首个下载任务");

    // 第二轮搜索（非洗版），已有 pending 任务在途，绝不可对同一集重复添加新种子
    let run2 = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{sub_id}/run"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(run2.status(), StatusCode::OK);
    assert_eq!(
        downloader.added().len(),
        1,
        "在途未完成的 pending 任务应占位，防多轮重复下载同集"
    );
}

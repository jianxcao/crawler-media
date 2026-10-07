use std::str::FromStr;
use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::http::StatusCode;
use domain::{AtomRule, Filter, FilterAtom, FilterId, SubscribeId};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use parking_lot::Mutex;
use serde_json::{Value, json};
use subscribe::QualityFact;
use tower::ServiceExt;

use super::common::{create_site, json_body, request};

#[tokio::test]
async fn legacy_run_uses_shared_keywords_and_wash_cut_ladder() {
    let tmp = tempfile::tempdir().unwrap();
    let urls = Arc::new(Mutex::new(Vec::new()));
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let app = router(
        ApiState::new(
            Store::open(tmp.path().join("data")).unwrap(),
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(UrlRecorder(urls.clone())),
            downloader.clone(),
            tmp.path().join("library"),
        )
        .unwrap(),
    );
    create_site(&app).await;

    let store = Store::open(tmp.path().join("data")).unwrap();
    let ladder = Filter {
        id: FilterId::new(),
        name: "strict wash ladder".into(),
        atoms: vec![FilterAtom {
            priority: 0,
            rule: AtomRule::UpgradeLadder("resolution,source".into()),
            exclude: false,
        }],
        keep_old_versions: false,
};
    store.insert_filter(&ladder).unwrap();
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/subscriptions",
            Some("management-secret"),
            json!({
                "media": {
                    "kind": "movie",
                    "title": "The Matrix: Special Edition",
                    "original_title": "The Matrix",
                    "year": 1999
                },
                "coverage": { "kind": "movie" },
                "fetch_mode": "search",
                "wash_cut": true,
                "wash_cut_filter_id": ladder.id.to_string()
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let id = created["data"]["id"].as_str().unwrap();
    let subscribe_id = SubscribeId::from_str(id).unwrap();
    let mut facts = subscribe::SubscribeFacts::default();
    facts.upsert(
        None,
        None,
        QualityFact {
            score: 1,
            path: Some("The.Matrix.1999.2160p.BluRay.x265-GROUP.mkv".into()),
        },
    );
    store.save_subscribe_facts(subscribe_id, &facts).unwrap();

    let run = app
        .oneshot(request(
            "POST",
            &format!("/api/v1/subscriptions/{id}/run"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::OK);

    let requests = urls.lock();
    assert_eq!(
        requests.len(),
        4,
        "all unique title/year searches should run: {requests:?}"
    );
    assert_eq!(
        requests
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );
    assert!(
        downloader.added().is_empty(),
        "an equal quality release must not replace the existing file"
    );
}

struct UrlRecorder(Arc<Mutex<Vec<String>>>);

impl Fetcher for UrlRecorder {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        self.0.lock().push(request.url.clone());
        Ok(super::common::nexusphp().to_string())
    }
}

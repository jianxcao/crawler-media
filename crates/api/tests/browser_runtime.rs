use std::sync::Arc;

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Site, SiteId};
use downloader::MemoryDownloader;
use indexer::{
    Browser, BrowserConfig, FetchRequest, Fetcher, IndexerError, PageSession, ProfileSet,
    RoutedFetcher,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

struct NoHttp;
impl Fetcher for NoHttp {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("fixture HTTP must not run".into()))
    }
}
struct FixturePage;
impl PageSession for FixturePage {
    fn goto(&self, _: &str) -> Result<(), IndexerError> {
        Ok(())
    }
    fn set_cookie_header(&self, _: &str) -> Result<(), IndexerError> {
        Ok(())
    }
    fn content(&self) -> Result<String, IndexerError> {
        Ok(include_str!("../../indexer/tests/fixtures/nexusphp.html").into())
    }
}

fn state(root: &std::path::Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "browser-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoHttp),
        Arc::new(MemoryDownloader::new(root.join("dl"))),
        root.join("library"),
    )
    .unwrap()
}
fn request(method: &str, body: Value) -> Request<Body> {
    request_at(method, "/api/v1/settings/browser", body)
}
fn request_at(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer browser-secret")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}
async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

type Endpoints = Arc<Mutex<Vec<String>>>;
fn routed_app(root: &std::path::Path, store: Arc<Mutex<Store>>) -> (axum::Router, Endpoints) {
    let overlay = root.join("profiles");
    std::fs::create_dir_all(&overlay).unwrap();
    std::fs::write(
        overlay.join("demo.yaml"),
        include_str!("../../indexer/src/profiles/demo.yaml")
            .replace("render: false", "render: true"),
    )
    .unwrap();
    let endpoints = Arc::new(Mutex::new(Vec::new()));
    let log = endpoints.clone();
    let browser = Browser::with_opener(BrowserConfig::disabled(), move |endpoint| {
        log.lock().push(endpoint.unwrap().to_owned());
        Ok(Arc::new(FixturePage))
    });
    let browser = api::runtime_browser::configure(browser, store.clone(), false).unwrap();
    let state = ApiState::new_arc(
        store,
        "browser-secret".into(),
        ProfileSet::load(Some(&overlay)).unwrap(),
        Arc::new(RoutedFetcher::new(NoHttp, browser)),
        Arc::new(MemoryDownloader::new(root.join("dl"))),
        root.join("library"),
    )
    .unwrap();
    (router(state), endpoints)
}
fn site(store: &Store, cdp_url: Option<&str>) -> Site {
    let site = Site {
        id: SiteId::new(),
        name: "fixture".into(),
        url: "https://fixture.invalid/".into(),
        profile_id: "demo".into(),
        cookie: None,
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: cdp_url.map(str::to_owned),
        downloader_id: None,
        enabled: true,
    };
    store.insert_site(&site).unwrap();
    site
}
async fn search(app: &axum::Router) -> Value {
    let response = app
        .clone()
        .oneshot(request_at(
            "GET",
            "/api/v1/search/torrents?keyword=Matrix",
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_body(response).await["data"].clone();
    if data["total"] == 0 {
        eprintln!("Browser search response: {data}");
    }
    data
}
async fn save(app: &axum::Router, payload: Value) {
    let response = app.clone().oneshot(request("PUT", payload)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_body(response).await["data"].clone();
    assert_eq!(data["obscura"]["usable"], false);
    assert_eq!(data["obscura"]["running"], Value::Null);
}

#[tokio::test]
async fn browser_settings_advertise_external_only_without_claiming_usable() {
    let tmp = tempfile::tempdir().unwrap();
    let response = router(state(tmp.path()))
        .oneshot(request("GET", json!({})))
        .await
        .unwrap();
    let data = json_body(response).await["data"].clone();
    assert_eq!(data["capabilities"]["mode"], "external-cdp-only");
    assert_eq!(data["capabilities"]["managed_chromium"], false);
    assert_eq!(data["obscura"]["usable"], false);
}

#[tokio::test]
async fn browser_settings_reject_enabled_obscura_without_valid_cdp() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(tmp.path());
    let app = router(state.clone());
    let response = app
        .oneshot(request("PUT", json!({"obscura":{"enabled":true,"url":""}})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        state
            .store()
            .lock()
            .get_setting(api::settings_keys::OBSCURA_ENABLED)
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn browser_settings_reject_websocket_discovery_and_managed_capability() {
    let tmp = tempfile::tempdir().unwrap();
    let app = router(state(tmp.path()));
    for payload in [
        json!({"cdp":{"enabled":true,"url":"ws://fixture.invalid:9222"}}),
        json!({"managed":{"enabled":true}}),
    ] {
        let response = app.clone().oneshot(request("PUT", payload)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn browser_settings_store_failure_is_not_success() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(tmp.path());
    let path = state.store().lock().sqlite_path().to_owned();
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch("CREATE TRIGGER reject_browser_setting BEFORE INSERT ON settings WHEN NEW.key = 'cdp.url' BEGIN SELECT RAISE(ABORT, 'fixture setting failure'); END;").unwrap();
    let response = router(state)
        .oneshot(request(
            "PUT",
            json!({"cdp":{"enabled":true,"url":"http://fixture.invalid:9222"}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn startup_obscura_and_saved_switches_reach_actual_indexer_fetcher() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path().join("data")).unwrap()));
    {
        let store = store.lock();
        store
            .put_setting(api::settings_keys::OBSCURA_ENABLED, "true")
            .unwrap();
        store
            .put_setting(
                api::settings_keys::OBSCURA_URL,
                "http://startup.invalid:9223",
            )
            .unwrap();
        site(&store, None);
    }
    let (app, endpoints) = routed_app(tmp.path(), store.clone());
    assert_eq!(search(&app).await["total"], 1);
    save(
        &app,
        json!({"cdp":{"enabled":true,"url":"http://global.invalid:9222"},
        "obscura":{"enabled":false}}),
    )
    .await;
    assert_eq!(search(&app).await["total"], 1);
    save(
        &app,
        json!({"obscura":{"enabled":true,"url":"http://saved.invalid:9333"}}),
    )
    .await;
    assert_eq!(search(&app).await["total"], 1);
    assert_eq!(
        &*endpoints.lock(),
        &[
            "http://startup.invalid:9223",
            "http://global.invalid:9222",
            "http://saved.invalid:9333"
        ]
    );

    // Rebuilding the real Fetcher from persisted settings gives the same endpoint as dynamic save.
    let (restarted, endpoints) = routed_app(tmp.path(), store);
    assert_eq!(search(&restarted).await["total"], 1);
    assert_eq!(&*endpoints.lock(), &["http://saved.invalid:9333"]);
}

#[tokio::test]
async fn site_cdp_is_authoritative_and_disabled_globals_do_not_leak_into_render() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path().join("data")).unwrap()));
    let mut site = site(&store.lock(), Some("http://site.invalid:9444"));
    let (app, endpoints) = routed_app(tmp.path(), store.clone());
    save(
        &app,
        json!({"cdp":{"enabled":true,"url":"http://global.invalid:9222"},
        "obscura":{"enabled":true,"url":"http://obscura.invalid:9223"}}),
    )
    .await;
    assert_eq!(search(&app).await["total"], 1);
    assert_eq!(&*endpoints.lock(), &["http://site.invalid:9444"]);
    save(
        &app,
        json!({"cdp":{"enabled":false},"obscura":{"enabled":false}}),
    )
    .await;
    assert_eq!(search(&app).await["total"], 1);
    site.cdp_url = None;
    store.lock().save_site(&site).unwrap();
    let data = search(&app).await;
    assert_eq!(data["total"], 0);
    assert!(data["sites"][0]["error"].as_str().unwrap().contains("CDP"));
    assert_eq!(endpoints.lock().len(), 2);
}

#[tokio::test]
async fn failed_save_restores_existing_route_and_does_not_report_usable() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path().join("data")).unwrap()));
    site(&store.lock(), None);
    let (app, endpoints) = routed_app(tmp.path(), store.clone());
    save(
        &app,
        json!({"obscura":{"enabled":true,"url":"http://old.invalid:9223"}}),
    )
    .await;
    let db = rusqlite::Connection::open(store.lock().sqlite_path()).unwrap();
    db.execute_batch("CREATE TRIGGER reject_browser_flag BEFORE INSERT ON settings WHEN NEW.key = 'obscura.enabled' BEGIN SELECT RAISE(ABORT, 'fixture flag failure'); END;").unwrap();
    let response = app
        .clone()
        .oneshot(request(
            "PUT",
            json!({"obscura":{"enabled":true,"url":"http://new.invalid:9223"}}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(search(&app).await["total"], 1);
    assert_eq!(&*endpoints.lock(), &["http://old.invalid:9223"]);
}

#[test]
fn startup_rejects_managed_and_incomplete_enabled_external_configuration() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path().join("data")).unwrap()));
    assert!(api::runtime_browser::production_browser(store.clone(), true).is_err());
    store
        .lock()
        .put_setting(api::settings_keys::OBSCURA_ENABLED, "1")
        .unwrap();
    assert!(api::runtime_browser::production_browser(store, false).is_err());
}

use api::{ApiState, Store};
use domain::{DownloaderId, Site, SiteId};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use std::sync::Arc;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline Site fixture".into()))
    }
}

pub struct Fixture {
    pub _tmp: tempfile::TempDir,
    pub state: ApiState,
    pub site: SiteId,
    pub downloader: DownloaderId,
}
impl Fixture {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state = ApiState::new(
            Store::open(tmp.path().join("data")).unwrap(),
            "site-downloader-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap();
        let site = SiteId::new();
        let downloader = DownloaderId::new();
        {
            let handle = state.store();
            let store = handle.lock();
            store
                .insert_site(&Site {
                    id: site,
                    name: "Original Site".into(),
                    url: "https://fixture.invalid/".into(),
                    profile_id: "mteam".into(),
                    cookie: Some("secret-cookie".into()),
                    api_key: None,
                    rss_url: None,
                    proxy: None,
                    rate_limit_per_minute: None,
                    cdp_url: None,
                    downloader_id: None,
                    enabled: true,
                    maintenance_via: domain::MaintenanceVia::Http,
                })
                .unwrap();
            store
                .insert_downloader(&store::DownloaderRow {
                    id: downloader,
                    name: "Original Downloader".into(),
                    kind: "unsupported".into(),
                    url: "http://fixture.invalid/".into(),
                    username: Some("admin".into()),
                    password: Some("secret-password".into()),
                    category: None,
                    path_maps: vec![],
                    is_default: false,
                    enabled: true,
                })
                .unwrap();
        }
        Self {
            _tmp: tmp,
            state,
            site,
            downloader,
        }
    }
    pub fn site_body(&self) -> Value {
        json!({"name":"Changed Site","url":"https://fixture.invalid/","profile_id":"mteam"})
    }
    pub fn downloader_body(&self) -> Value {
        json!({"name":"Changed Downloader","kind":"qbittorrent","url":"http://fixture.invalid/"})
    }
    pub fn site_uri(&self, suffix: &str) -> String {
        format!("/api/v1/sites/{}{suffix}", self.site)
    }
    pub fn downloader_uri(&self, suffix: &str) -> String {
        format!("/api/v1/downloaders/{}{suffix}", self.downloader)
    }
}

pub fn route(f: &Fixture, route: usize) -> (&'static str, String, Value, u16) {
    match route {
        0 => ("GET", "/api/v1/sites".into(), Value::Null, 200),
        1 => ("GET", f.site_uri(""), Value::Null, 200),
        2 => ("GET", "/api/v1/downloaders".into(), Value::Null, 200),
        3 => ("GET", f.downloader_uri(""), Value::Null, 200),
        4 => (
            "GET",
            "/api/v1/downloaders/target-prefs".into(),
            Value::Null,
            200,
        ),
        5 => ("POST", "/api/v1/sites".into(), f.site_body(), 201),
        6 => ("PUT", f.site_uri(""), f.site_body(), 200),
        7 => ("PATCH", f.site_uri(""), json!({"name":"Changed Site"}), 200),
        8 => ("DELETE", f.site_uri(""), Value::Null, 200),
        9 => ("PATCH", f.site_uri("/boost"), json!({"enabled":true}), 200),
        10 => (
            "POST",
            "/api/v1/downloaders".into(),
            f.downloader_body(),
            201,
        ),
        11 => (
            "PATCH",
            f.downloader_uri(""),
            json!({"name":"Changed Downloader"}),
            200,
        ),
        12 => ("DELETE", f.downloader_uri(""), Value::Null, 200),
        13 => (
            "PUT",
            "/api/v1/downloaders/target-prefs/movie".into(),
            json!({"kind":"default"}),
            200,
        ),
        14 => ("POST", f.downloader_uri("/verify"), Value::Null, 400),
        15 => ("POST", f.site_uri("/verify"), Value::Null, 200),
        16 => ("POST", f.site_uri("/login"), Value::Null, 400),
        17 => ("POST", f.site_uri("/check-in"), Value::Null, 400),
        18 => ("GET", "/api/v1/sites/boost-stats".into(), Value::Null, 200),
        19 => ("GET", "/api/v1/downloaders/tasks".into(), Value::Null, 200),
        20 => (
            "POST",
            "/api/v1/downloaders/tasks/remove".into(),
            json!({"task_id":"missing"}),
            404,
        ),
        21 => (
            "POST",
            "/api/v1/downloaders/tasks/replace".into(),
            json!({"task_id":"missing"}),
            404,
        ),
        _ => (
            "POST",
            "/api/v1/downloaders/submit".into(),
            json!({"download_url":"https://fixture.invalid/torrent","title":"Fixture.2026","downloader_id":f.downloader}),
            400,
        ),
    }
}

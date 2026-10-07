use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex};

use domain::{Site, SiteId};
use indexer::{
    Browser, BrowserConfig, FetchRequest, Fetcher, Indexer, IndexerError, PageSession, ProfileSet,
    RecordingFetcher, RoutedFetcher,
};

fn site_with_profile(profile_id: &str, cdp_url: Option<&str>) -> Site {
    Site {
        id: SiteId::new(),
        name: profile_id.into(),
        url: "https://pt.example/".into(),
        profile_id: profile_id.into(),
        cookie: Some("uid=1".into()),
        api_key: None,
        rss_url: Some("https://pt.example/rss".into()),
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: cdp_url.map(str::to_string),
        downloader_id: None,
        enabled: true,
    }
}

struct Capture {
    requests: Mutex<Vec<FetchRequest>>,
    html: String,
}

impl Fetcher for Capture {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        self.requests.lock().unwrap().push(FetchRequest {
            key: request.key.clone(),
            url: request.url.clone(),
            method: request.method,
            body: request.body.clone(),
            cookie: request.cookie.clone(),
            api_key: request.api_key.clone(),
            proxy: request.proxy.clone(),
            render: request.render,
            cdp_url: request.cdp_url.clone(),
        });
        Ok(self.html.clone())
    }
}

struct FakePage {
    html: String,
    cookies: Option<String>,
    navigated: Mutex<Vec<String>>,
}

impl PageSession for FakePage {
    fn goto(&self, url: &str) -> Result<(), IndexerError> {
        self.navigated.lock().unwrap().push(url.to_string());
        Ok(())
    }

    fn set_cookie_header(&self, header: &str) -> Result<(), IndexerError> {
        assert_eq!(header, self.cookies.clone().unwrap_or_default());
        Ok(())
    }

    fn content(&self) -> Result<String, IndexerError> {
        Ok(self.html.clone())
    }
}

fn write_render_overlay(dir: &std::path::Path) {
    fs::write(
        dir.join("demo.yaml"),
        r#"
id: demo
framework: nexusphp
render: true
search:
  path: /torrents.php
  query_param: search
list:
  item: "tr.torrent"
fields:
  title:
    selector: "a.name"
    attr: title
  enclosure:
    selector: "a.download"
    attr: href
  size:
    selector: "td.size"
  seeders:
    selector: "td.seeders"
  free:
    selector: "img.pro_free"
  hr:
    selector: "img.hitandrun"
rss:
  item: item
  title:
    selector: title
  enclosure:
    selector: enclosure
    attr: url
"#,
    )
    .unwrap();
}

#[test]
fn render_search_uses_browser_html_and_same_selectors() {
    let overlay = tempfile::tempdir().unwrap();
    write_render_overlay(overlay.path());
    let html = include_str!("fixtures/nexusphp.html").to_string();
    let page = Arc::new(FakePage {
        html: html.clone(),
        cookies: Some("uid=1".into()),
        navigated: Mutex::new(Vec::new()),
    });
    let opened = Arc::new(Mutex::new(Vec::new()));
    let opened_log = opened.clone();
    let page_for_open = page.clone();
    let browser = Browser::with_opener(BrowserConfig::disabled(), move |cdp_url: Option<&str>| {
        opened_log.lock().unwrap().push(cdp_url.map(str::to_string));
        Ok(page_for_open.clone() as Arc<dyn PageSession>)
    });
    let http = Capture {
        requests: Mutex::new(Vec::new()),
        html: "HTTP BODY MUST NOT BE PARSED".into(),
    };
    let fetcher = RoutedFetcher::new(http, browser);
    let indexer = Indexer::new(
        ProfileSet::load(Some(overlay.path())).unwrap(),
        Arc::new(fetcher),
    );
    let site = site_with_profile("demo", Some("ws://127.0.0.1:9222"));

    let outcome = indexer.search(&[site.clone()], "matrix");

    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.torrents.len(), 1);
    assert_eq!(
        outcome.torrents[0].title,
        "The.Matrix.1999.2160p.BluRay.x265-GROUP"
    );
    assert_eq!(
        outcome.torrents[0].enclosure,
        "https://pt.example/download.php?id=1&passkey=abc"
    );
    assert_eq!(
        *page.navigated.lock().unwrap(),
        vec!["https://pt.example/torrents.php?search=matrix".to_string()]
    );
    assert_eq!(
        opened.lock().unwrap().as_slice(),
        &[Some("ws://127.0.0.1:9222".into())]
    );
}

#[test]
fn http_search_does_not_open_browser() {
    let html = include_str!("fixtures/nexusphp.html").to_string();
    let http = Capture {
        requests: Mutex::new(Vec::new()),
        html,
    };
    let opened = Arc::new(Mutex::new(0usize));
    let opened_log = opened.clone();
    let browser = Browser::with_opener(BrowserConfig::disabled(), move |_cdp| {
        *opened_log.lock().unwrap() += 1;
        Err(IndexerError::Fetch("browser must not open".into()))
    });
    let indexer = Indexer::new(
        ProfileSet::load(None).unwrap(),
        Arc::new(RoutedFetcher::new(http, browser)),
    );
    let site = site_with_profile("demo", None);
    let outcome = indexer.search(&[site], "matrix");
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.torrents.len(), 1);
    assert_eq!(*opened.lock().unwrap(), 0);
}

#[test]
fn default_browser_config_has_no_chromium_and_is_headless_when_enabled() {
    let data = tempfile::tempdir().unwrap();
    let off = BrowserConfig::disabled();
    assert!(!off.enabled);
    assert!(!off.chromium_present());
    assert!(off.headless);

    let on = BrowserConfig::enable_in(data.path()).unwrap();
    assert!(on.enabled);
    assert!(on.headless);
    assert!(on.chromium_present());
    assert!(on.chromium_path().starts_with(data.path()));
    assert!(!data.path().join("chromium").as_os_str().is_empty());
}

#[test]
fn recording_fetcher_never_launches_chromium_for_injected_html() {
    let overlay = tempfile::tempdir().unwrap();
    write_render_overlay(overlay.path());
    let html = include_str!("fixtures/nexusphp.html");
    let site = site_with_profile("demo", Some("ws://example"));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let fetcher = RecordingFetcher::new(
        HashMap::from([(format!("search:{}", site.id), Ok(html.to_string()))]),
        requests.clone(),
    );
    let indexer = Indexer::new(
        ProfileSet::load(Some(overlay.path())).unwrap(),
        Arc::new(fetcher),
    );
    let outcome = indexer.search(&[site], "matrix");
    assert_eq!(outcome.torrents.len(), 1);
    let captured = requests.lock().unwrap();
    assert!(captured[0].render);
    assert_eq!(captured[0].cdp_url.as_deref(), Some("ws://example"));
}

#[test]
fn browser_with_external_cdp_attempts_real_target_connection() {
    let browser = Browser::new(BrowserConfig::disabled());
    let req = FetchRequest {
        key: "test".into(),
        url: "https://example.com".into(),
        method: indexer::FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        render: true,
        cdp_url: Some("http://127.0.0.1:54322".into()),
    };
    let err = browser.fetch_html(&req).unwrap_err();
    let msg = err.to_string();
    assert!(
        !msg.contains("no PageSession opener is configured"),
        "生产路径已接通真实 CDP 会话创建，不应再报告缺少测试 opener: {msg}"
    );
    assert!(
        msg.contains("无法连接外部 CDP 端点"),
        "端点不可达时应返回明确的网络连接错误: {msg}"
    );
}

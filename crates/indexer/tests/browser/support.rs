//! Fixtures shared by every browser test module: a recording `Fetcher`, a fake page
//! session, the render-overlay writer and the request builder.

pub(crate) use std::collections::HashMap;
pub(crate) use std::fs;
pub(crate) use std::io::{Read, Write};
pub(crate) use std::sync::{Arc, Mutex};
pub(crate) use std::time::Instant;

pub(crate) use domain::{Site, SiteId};
pub(crate) use indexer::{
    Browser, BrowserConfig, FetchMethod, FetchRequest, Indexer, IndexerError, PageSession,
    ProfileSet, RecordingFetcher,
};
pub(crate) fn site_with_profile(profile_id: &str, cdp_url: Option<&str>) -> Site {
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
        maintenance_via: domain::MaintenanceVia::Http,
    }
}
pub(crate) fn write_render_overlay(dir: &std::path::Path) {
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
pub(crate) fn plain_req(proxy: Option<&str>) -> FetchRequest {
    FetchRequest {
        key: "t".into(),
        url: "https://example.org/".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: Some("a=b".into()),
        api_key: None,
        proxy: proxy.map(Into::into),
        cdp_url: Some("http://127.0.0.1:9222".into()),
    }
}

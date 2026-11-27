use std::collections::HashMap;
use std::sync::Arc;

use domain::{MaintenanceVia, Site, SiteId};
use indexer::{FetchRequest, Fetcher, Indexer, IndexerError, RegistrySnapshot};
use parking_lot::Mutex;

fn nexus_site(profile_id: &str, tz: &str) -> Site {
    Site {
        id: SiteId::new(),
        name: profile_id.into(),
        url: "https://pt.example/".into(),
        profile_id: profile_id.into(),
        cookie: Some("c_secure_uid=1; c_secure_pass=ok".into()),
        api_key: None,
        rss_url: Some("https://pt.example/torrentrss.php".into()),
        proxy: None,
        rate_limit_per_minute: Some(600),
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: MaintenanceVia::Http,
        browser_search: false,
        timezone: tz.into(),
    }
}

struct MapFetcher(Mutex<HashMap<String, Result<String, String>>>);

impl Fetcher for MapFetcher {
    fn fetch(&self, req: &FetchRequest) -> Result<String, IndexerError> {
        let key = if req.url.ends_with("/index.php") || req.url.ends_with("index.php") {
            "index".to_string()
        } else {
            req.key.clone()
        };
        match self.0.lock().get(&key).cloned() {
            Some(Ok(body)) => Ok(body),
            Some(Err(msg)) => Err(IndexerError::Fetch(msg)),
            None => Err(IndexerError::Fetch(format!("missing fixture for {key}"))),
        }
    }
}

#[test]
fn html_parser_normalizes_categories_extracts_free_2x_and_avoids_upload_time_as_free_until() {
    let site = nexus_site("audiences", "Asia/Shanghai");
    let html = r#"<!doctype html><html><body>
<div class="site-userbar__name" data-uploader-actions="logout.php">user</div>
<table class="torrents"><tbody>
<tr>
  <td><a href="?cat=402">剧集</a></td>
  <td>
    <a href="details.php?id=101">Show.S01E01.1080p</a>
    <img class="pro_free2up" alt="2X Free" />
    <span title="2026-11-28 20:00:00">剩余：1天</span>
    <a href="download.php?id=101">dl</a>
  </td>
  <td></td>
  <td><span title="2026-11-27 10:00:00">3小时</span></td>
  <td>20.00 GB</td>
  <td>15</td>
  <td>3</td>
  <td>20</td>
</tr>
<tr>
  <td><a href="?cat=401">电影</a></td>
  <td>
    <a href="details.php?id=102">Movie.2026.2160p</a>
    <img class="pro_free" alt="Free" />
    <a href="download.php?id=102">dl</a>
  </td>
  <td></td>
  <td><span title="2026-11-20 09:00:00">7天</span></td>
  <td>10.00 GB</td>
  <td>8</td>
  <td>1</td>
  <td>5</td>
</tr>
</tbody></table></body></html>"#;

    let indexer = Indexer::new(
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(MapFetcher(Mutex::new(HashMap::from([(
            format!("search:{}", site.id),
            Ok(html.to_string()),
        )])))),
    );
    let outcome = indexer.search(&[site], "");
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.torrents.len(), 2);

    let tv = &outcome.torrents[0];
    assert_eq!(tv.category.as_deref(), Some("tv"));
    assert!(tv.free);
    assert!(tv.free_2x);
    // 2026-11-28 20:00:00 Asia/Shanghai (+08:00) == 2026-11-28T12:00:00Z = 1795867200
    assert_eq!(tv.free_until, Some(1_795_867_200));

    let movie = &outcome.torrents[1];
    assert_eq!(movie.category.as_deref(), Some("movie"));
    assert!(movie.free);
    assert!(!movie.free_2x);
    assert_eq!(
        movie.free_until, None,
        "upload_time span[title] must never be mistaken for free_until"
    );
}

#[test]
fn rss_parser_normalizes_common_category_labels() {
    let site = nexus_site("demo", "UTC");
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>demo</title>
  <item><title>Series.S01E01</title><enclosure url="https://pt.example/download.php?id=1" length="1024" /><category>剧集</category><free>1</free></item>
  <item><title>Anime.EP01</title><enclosure url="https://pt.example/download.php?id=2" length="1024" /><category>Anime</category><free>yes</free></item>
  <item><title>Doc.Film</title><enclosure url="https://pt.example/download.php?id=3" length="1024" /><category>纪录片</category><free>true</free></item>
  <item><title>Album.FLAC</title><enclosure url="https://pt.example/download.php?id=4" length="1024" /><category>音乐</category></item>
</channel></rss>"#;

    let indexer = Indexer::new(
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(MapFetcher(Mutex::new(HashMap::from([(
            format!("rss:{}", site.id),
            Ok(xml.to_string()),
        )])))),
    );
    let out = indexer.rss(&[site]);
    assert!(out.failures.is_empty(), "{:?}", out.failures);
    let cats: Vec<_> = out.torrents.iter().map(|t| t.category.as_deref()).collect();
    assert_eq!(
        cats,
        vec![
            Some("tv"),
            Some("anime"),
            Some("documentary"),
            Some("music")
        ]
    );
}

#[test]
fn fetch_user_stats_parses_nexusphp_chinese_english_and_auth_failures() {
    let site = nexus_site("audiences", "UTC");
    let bodies = Arc::new(MapFetcher(Mutex::new(HashMap::from([(
        "index".into(),
        Ok(r#"<html><body>
<div class="site-userbar__name" data-uploader-actions="logout.php">alice</div>
<table id="info_block"><tr><td>
  <span>分享率: 3.000</span>
  <span>上传量: 1,024.00 GiB</span>
  <span>下载量: 512.00 GiB</span>
</td></tr></table></body></html>"#
            .into()),
    )]))));
    let indexer = Indexer::new(RegistrySnapshot::builtin().unwrap(), bodies.clone());
    let stats = indexer.fetch_user_stats(&site).unwrap().unwrap();
    assert_eq!(
        stats,
        (
            1024 * 1024 * 1024 * 1024, // 1024 GiB = 1 TiB
            512 * 1024 * 1024 * 1024,
        )
    );

    bodies.0.lock().insert(
        "index".into(),
        Ok(r#"<html><body><a href="logout.php">Logout</a>
        <div id="info_block">Uploaded: 2.00 TB &nbsp; Downloaded: 1.00 TB</div>
        </body></html>"#
            .into()),
    );
    let en = indexer.fetch_user_stats(&site).unwrap().unwrap();
    assert_eq!(en, (2 * 1024_u64.pow(4), 1024_u64.pow(4)));

    bodies.0.lock().insert(
        "index".into(),
        Ok(r#"<html><body><form action="takelogin.php"><input name="password"/></form></body></html>"#
            .into()),
    );
    assert!(indexer.fetch_user_stats(&site).is_err());

    bodies.0.lock().insert(
        "index".into(),
        Ok("<html><body><div>no stats</div><a href='logout.php'>out</a></body></html>".into()),
    );
    assert_eq!(indexer.fetch_user_stats(&site).unwrap(), None);
}

use std::sync::Arc;

use domain::{Site, SiteId, Torrent};
use indexer::{
    FetchMethod, FetchRequest, Fetcher, Framework, Indexer, IndexerError, RegistrySnapshot,
    ResolvedDownload,
};
use parking_lot::Mutex;

fn hdarea_site(url: &str, api_key: Option<&str>, cookie: Option<&str>) -> Site {
    Site {
        id: SiteId::new(),
        name: "HDArea".into(),
        url: url.into(),
        profile_id: "hdarea".into(),
        cookie: cookie.map(str::to_string),
        api_key: api_key.map(str::to_string),
        rss_url: Some("https://hdarea.club/torrentrss.php".into()),
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
        browser_search: false,
        timezone: "UTC".into(),
    }
}

const LIVE_SEARCH_JSON: &str = r#"{
  "success": true,
  "code": 0,
  "http_code": 200,
  "message": "OK",
  "data": {
    "items": [
      {
        "id": 469752,
        "name": "Knowing Bros S01E552 2015 1080p Viu WEB-DL H264 AAC-ADWeb",
        "small_descr": "认识的哥哥 第552期 中字",
        "category_id": 403,
        "category_name": "TV Shows",
        "size": 2465419469,
        "seeders": 8,
        "leechers": 7,
        "times_completed": 19,
        "added": "2026-11-22 14:57:37",
        "sp_state": 2,
        "sp_state_name": "free",
        "download_volume_factor": 0.0,
        "is_free": true,
        "imdb_id": "tt5247622",
        "poster_url": "https://hdarea.club/posters/469752.jpg",
        "detail_url": "https://hdarea.club/details.php?id=469752",
        "download_url": "https://hdarea.club/download.php?id=469752&dl_token=searchtok"
      },
      {
        "id": 469753,
        "name": "Dune Part Two 2024 2160p BluRay REMUX HEVC-HDATeam",
        "small_descr": "",
        "category_id": 401,
        "category_name": "Movies UHD",
        "size": 68719476736,
        "seeders": 25,
        "leechers": 2,
        "times_completed": 110,
        "added": "2026-11-21 10:00:00",
        "sp_state": 4,
        "sp_state_name": "2xfree",
        "download_volume_factor": 1.0,
        "is_free": false,
        "download_url": ""
      },
      {
        "id": 469754,
        "name": "Normal Promo 2024 1080p BluRay x264-GROUP",
        "small_descr": "普通50%促销",
        "category_id": 415,
        "category_name": "Movies",
        "size": 10737418240,
        "seeders": 3,
        "leechers": 1,
        "times_completed": 5,
        "added": "2026-11-20 09:00:00",
        "sp_state": 5,
        "sp_state_name": "50%",
        "download_volume_factor": 0.5,
        "is_free": false
      }
    ],
    "pagination": { "page": 0, "per_page": 100, "total": 361309 }
  }
}"#;

#[test]
fn hdarea_profile_declares_hybrid_auth_and_api_check_in() {
    let profiles = RegistrySnapshot::builtin().unwrap();
    let hdarea = profiles.profile("hdarea").unwrap();
    assert_eq!(hdarea.framework, Framework::Api);
    assert_eq!(hdarea.effective_auth_types(), vec!["apikey", "cookie"]);
    assert_eq!(
        hdarea.api_check_in_path.as_deref(),
        Some("/api/v1/user/signin")
    );
    assert!(hdarea.supports_check_in());
    assert_eq!(hdarea.effective_site_base_url(""), "https://hdarea.club");
    assert_eq!(
        hdarea.effective_site_base_url("https://www.hdarea.co/"),
        "https://www.hdarea.co"
    );
    assert_eq!(
        hdarea.effective_site_base_url("http://127.0.0.1:19876/"),
        "http://127.0.0.1:19876"
    );

    let mteam = profiles.profile("mteam").unwrap();
    assert_eq!(mteam.effective_auth_types(), vec!["apikey"]);
    assert!(!mteam.supports_check_in());

    let pterclub = profiles.profile("pterclub").unwrap();
    assert_eq!(pterclub.effective_auth_types(), vec!["cookie"]);
    assert!(pterclub.supports_check_in());
}

#[test]
fn hdarea_api_search_builds_rest_v1_get_and_parses_items() {
    struct Stub;
    impl Fetcher for Stub {
        fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
            assert_eq!(request.method, FetchMethod::Get);
            assert_eq!(
                request.api_key.as_deref(),
                Some("0123456789abcdef0123456789abcdef")
            );
            assert_eq!(
                request.url,
                "https://hdarea.club/api/v1/torrents?incldead=1&per_page=100&page=1&q=Knowing%20Bros&cat=402,403"
            );
            Ok(LIVE_SEARCH_JSON.into())
        }
    }

    let profiles = RegistrySnapshot::builtin().unwrap();
    let indexer = Indexer::new(profiles, Arc::new(Stub));
    let site = hdarea_site("", Some("0123456789abcdef0123456789abcdef"), None);
    let outcome = indexer.search_page_categories(&[site], "Knowing Bros", 2, &["tv".into()]);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.torrents.len(), 3);

    let first = &outcome.torrents[0];
    assert_eq!(first.id.as_deref(), Some("469752"));
    assert_eq!(
        first.title,
        "Knowing Bros S01E552 2015 1080p Viu WEB-DL H264 AAC-ADWeb [认识的哥哥 第552期 中字]"
    );
    assert_eq!(first.size_bytes, Some(2_465_419_469));
    assert_eq!(first.seeders, Some(8));
    assert_eq!(first.leechers, Some(7));
    assert_eq!(first.snatched, Some(19));
    assert_eq!(first.upload_time.as_deref(), Some("2026-11-22 14:57:37"));
    assert!(first.free);
    assert_eq!(first.imdb_id.as_deref(), Some("tt5247622"));
    assert_eq!(
        first.poster_url.as_deref(),
        Some("https://hdarea.club/posters/469752.jpg")
    );
    assert_eq!(
        first.detail_url.as_deref(),
        Some("https://hdarea.club/details.php?id=469752")
    );
    assert_eq!(first.category.as_deref(), Some("TV Shows"));
    assert_eq!(
        first.enclosure,
        "https://hdarea.club/download.php?id=469752&dl_token=searchtok"
    );

    let second = &outcome.torrents[1];
    assert_eq!(
        second.title,
        "Dune Part Two 2024 2160p BluRay REMUX HEVC-HDATeam"
    );
    assert!(second.free, "sp_state=4 (2xfree) must count as free");
    assert_eq!(
        second.enclosure, "https://hdarea.club/download.php?id=469753",
        "empty download_url must fall back to base/download.php?id=..."
    );

    let third = &outcome.torrents[2];
    assert!(!third.free, "50% promo must not be marked free");
}

#[test]
fn hdarea_whitespace_keyword_omits_q_param_for_verify_probe() {
    struct BlankStub;
    impl Fetcher for BlankStub {
        fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
            assert_eq!(
                request.url,
                "http://127.0.0.1:28080/api/v1/torrents?incldead=1&per_page=100&page=0"
            );
            Ok(LIVE_SEARCH_JSON.into())
        }
    }

    let indexer = Indexer::new(RegistrySnapshot::builtin().unwrap(), Arc::new(BlankStub));
    let site = hdarea_site("http://127.0.0.1:28080/", Some("hexkey"), None);
    let outcome = indexer.search(&[site], "   ");
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.torrents.len(), 3);
}

#[test]
fn hdarea_error_envelopes_distinguish_auth_401_and_business_errors() {
    struct ErrStub(Mutex<String>);
    impl Fetcher for ErrStub {
        fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
            Ok(self.0.lock().clone())
        }
    }

    let stub = Arc::new(ErrStub(Mutex::new(
        r#"{"success":false,"code":401,"http_code":401,"message":"Invalid API Key"}"#.into(),
    )));
    let indexer = Indexer::new(RegistrySnapshot::builtin().unwrap(), stub.clone());
    let site = hdarea_site("https://hdarea.club", Some("badkey"), None);

    let auth_out = indexer.search(std::slice::from_ref(&site), "dune");
    assert_eq!(auth_out.failures.len(), 1);
    assert!(
        auth_out.failures[0].error.contains("Invalid API Key")
            && !auth_out.failures[0].error.starts_with("parse:"),
        "expected non-parse auth error, got: {}",
        auth_out.failures[0].error
    );

    *stub.0.lock() =
        r#"{"success":false,"code":500,"http_code":500,"message":"Database overloaded"}"#.into();
    let biz_out = indexer.search(&[site], "dune");
    assert_eq!(biz_out.failures.len(), 1);
    assert!(
        biz_out.failures[0].error.contains("Database overloaded"),
        "got: {}",
        biz_out.failures[0].error
    );
}

#[test]
fn hdarea_resolve_torrent_download_handles_string_and_object_shapes() {
    struct DlStub(Mutex<String>);
    impl Fetcher for DlStub {
        fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
            assert_eq!(request.method, FetchMethod::PostJson);
            assert_eq!(
                request.url,
                "https://hdarea.club/api/v1/torrents/genDlToken"
            );
            assert_eq!(request.api_key.as_deref(), Some("valid-key"));
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(request.body.as_deref().unwrap())
                    .unwrap(),
                serde_json::json!({ "id": "469752" })
            );
            Ok(self.0.lock().clone())
        }
    }

    let stub = Arc::new(DlStub(Mutex::new(
        r#"{"success":true,"code":0,"data":"https://hdarea.club/download.php?id=469752&dl_token=str_tok"}"#.into(),
    )));
    let indexer = Indexer::new(RegistrySnapshot::builtin().unwrap(), stub.clone());
    let site = hdarea_site("https://hdarea.club", Some("valid-key"), None);
    let torrent = Torrent {
        site_id: site.id,
        title: "Test".into(),
        enclosure: "https://hdarea.club/download.php?id=469752".into(),
        size_bytes: None,
        seeders: None,
        free: true,
        hr: None,
        imdb_id: None,
        id: Some("469752".into()),
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    };

    assert_eq!(
        indexer.resolve_torrent_download(&site, &torrent).unwrap(),
        Some(ResolvedDownload::Url(
            "https://hdarea.club/download.php?id=469752&dl_token=str_tok".into()
        ))
    );

    *stub.0.lock() = r#"{"success":true,"code":"0","data":{"download_url":"https://hdarea.club/download.php?id=469752&dl_token=obj_dl"}}"#.into();
    assert_eq!(
        indexer.resolve_torrent_download(&site, &torrent).unwrap(),
        Some(ResolvedDownload::Url(
            "https://hdarea.club/download.php?id=469752&dl_token=obj_dl".into()
        ))
    );

    *stub.0.lock() =
        r#"{"success":true,"code":0,"data":{"url":"/download.php?id=469752&dl_token=rel_url"}}"#
            .into();
    assert_eq!(
        indexer.resolve_torrent_download(&site, &torrent).unwrap(),
        Some(ResolvedDownload::Url(
            "https://hdarea.club/download.php?id=469752&dl_token=rel_url".into()
        ))
    );
}

#[test]
fn hdarea_cookie_without_apikey_falls_back_to_nexusphp_html_search() {
    const HTML_10_COL: &str = r#"<!doctype html>
<html><body>
<a href="logout.php">退出</a>
<table class="torrents">
  <tbody>
    <tr>
      <td><a href="?cat=401"><img alt="Movies" /></a></td>
      <td>
        <table class="torrentname"><tr><td>
          <a href="details.php?id=888"><b>Matrix Resurrections 2021 2160p BluRay x265-HDA</b></a>
          <img class="pro_free" />
        </td><td><a href="download.php?id=888&passkey=pk">DL</a></td></tr></table>
      </td>
      <td>0</td>
      <td>0</td>
      <td><span title="2026-11-20 18:00:00">2d</span></td>
      <td>12.5 GB</td>
      <td>15</td>
      <td>3</td>
      <td>88</td>
      <td>anon</td>
    </tr>
  </tbody>
</table>
</body></html>"#;

    struct CookieStub;
    impl Fetcher for CookieStub {
        fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
            assert_eq!(request.method, FetchMethod::Get);
            assert_eq!(
                request.url,
                "https://hdarea.club/torrents.php?search=matrix&cat402=1&cat403=1"
            );
            assert_eq!(request.cookie.as_deref(), Some("c_secure_uid=7"));
            assert_eq!(request.api_key, None);
            Ok(HTML_10_COL.into())
        }
    }

    let indexer = Indexer::new(RegistrySnapshot::builtin().unwrap(), Arc::new(CookieStub));
    let site = hdarea_site("https://hdarea.club", None, Some("c_secure_uid=7"));
    let outcome =
        indexer.search_page_categories(std::slice::from_ref(&site), "matrix", 1, &["tv".into()]);
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(outcome.torrents.len(), 1);
    let t = &outcome.torrents[0];
    assert_eq!(t.title, "Matrix Resurrections 2021 2160p BluRay x265-HDA");
    assert_eq!(
        t.enclosure,
        "https://hdarea.club/download.php?id=888&passkey=pk"
    );
    assert_eq!(t.seeders, Some(15));
    assert_eq!(t.leechers, Some(3));
    assert_eq!(t.snatched, Some(88));
    assert!(t.free);
    assert_eq!(
        indexer.resolve_torrent_download(&site, t).unwrap(),
        None,
        "cookie fallback enclosure should not call API genDlToken"
    );
}

#[test]
fn hdarea_resolve_torrent_download_short_circuits_existing_dl_token_and_passkey() {
    struct PanicStub;
    impl Fetcher for PanicStub {
        fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
            panic!("unexpected genDlToken call to {}", request.url);
        }
    }

    let indexer = Indexer::new(RegistrySnapshot::builtin().unwrap(), Arc::new(PanicStub));
    let site = hdarea_site("https://hdarea.club", Some("valid-key"), None);
    let mut torrent = Torrent {
        site_id: site.id,
        title: "Knowing Bros S01E552".into(),
        enclosure: "https://hdarea.club/download.php?id=469752&dl_token=searchtok".into(),
        size_bytes: None,
        seeders: None,
        free: true,
        hr: None,
        imdb_id: None,
        id: Some("469752".into()),
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    };

    assert_eq!(
        indexer.resolve_torrent_download(&site, &torrent).unwrap(),
        None,
        "already-signed dl_token enclosure must be used directly without genDlToken"
    );

    torrent.enclosure = "https://hdarea.club/download.php?id=469752&passkey=pk123".into();
    assert_eq!(
        indexer.resolve_torrent_download(&site, &torrent).unwrap(),
        None,
        "already-signed passkey enclosure must be used directly without genDlToken"
    );
}

#[test]
fn hdarea_api_mode_omits_stale_cookie_in_search_and_download_requests() {
    struct InspectCookieStub;
    impl Fetcher for InspectCookieStub {
        fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
            assert_eq!(
                request.cookie, None,
                "API mode request to {} must not send stale cookie alongside x-api-key",
                request.url
            );
            if request.url.contains("genDlToken") {
                Ok(r#"{"success":true,"code":0,"data":"https://hdarea.club/download.php?id=469753&dl_token=fresh"}"#.into())
            } else {
                Ok(LIVE_SEARCH_JSON.into())
            }
        }
    }

    let indexer = Indexer::new(
        RegistrySnapshot::builtin().unwrap(),
        Arc::new(InspectCookieStub),
    );
    let site = hdarea_site(
        "https://hdarea.club",
        Some("0123456789abcdef0123456789abcdef"),
        Some("c_secure_uid=stale"),
    );

    let outcome = indexer.search(std::slice::from_ref(&site), "dune");
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);

    let unsigned = &outcome.torrents[1];
    assert_eq!(
        indexer.resolve_torrent_download(&site, unsigned).unwrap(),
        Some(ResolvedDownload::Url(
            "https://hdarea.club/download.php?id=469753&dl_token=fresh".into()
        ))
    );
}

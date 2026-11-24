use std::sync::Arc;

use domain::{Site, SiteId, Torrent};
use indexer::{FetchRequest, Fetcher, Indexer, IndexerError, ProfileSet};
use parking_lot::Mutex;

fn make_site(profile_id: &str, url: &str, api_key: Option<&str>, cookie: Option<&str>) -> Site {
    Site {
        id: SiteId::new(),
        name: profile_id.into(),
        url: url.into(),
        profile_id: profile_id.into(),
        cookie: cookie.map(str::to_string),
        api_key: api_key.map(str::to_string),
        rss_url: Some(format!("{}/torrentrss.php", url.trim_end_matches('/'))),
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
    }
}

#[test]
fn mteam_search_rejects_false_success_and_numeric_code_envelopes() {
    struct BodyStub(Mutex<String>);
    impl Fetcher for BodyStub {
        fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
            Ok(self.0.lock().clone())
        }
    }

    let stub = Arc::new(BodyStub(Mutex::new(String::new())));
    let indexer = Indexer::new(ProfileSet::load(None).unwrap(), stub.clone());
    let site = make_site("mteam", "https://api.m-team.cc", Some("bad-key"), None);

    for (envelope, expected_fragment) in [
        (
            r#"{"success":false,"message":"Invalid API Key"}"#,
            "Invalid API Key",
        ),
        (
            r#"{"success":"false","code":"0","message":"Token expired","data":{"data":[{"id":"1","name":"Should.Not.Return"}]}}"#,
            "Token expired",
        ),
        (r#"{"code":401,"message":"bad key"}"#, "401"),
        (
            r#"{"code":"429","message":"請求過於頻繁","data":""}"#,
            "請求過於頻繁",
        ),
    ] {
        *stub.0.lock() = envelope.into();
        let outcome = indexer.search(std::slice::from_ref(&site), "dune");
        assert!(
            outcome.torrents.is_empty(),
            "error envelope must not yield torrents: {envelope}"
        );
        assert_eq!(
            outcome.failures.len(),
            1,
            "error envelope must fail search instead of returning empty list: {envelope}"
        );
        assert!(
            outcome.failures[0].error.contains(expected_fragment),
            "expected '{expected_fragment}' in '{}'",
            outcome.failures[0].error
        );
    }
}

#[test]
fn mteam_download_rejects_error_envelopes_before_inspecting_data() {
    struct BodyStub(Mutex<String>);
    impl Fetcher for BodyStub {
        fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
            Ok(self.0.lock().clone())
        }
    }

    let stub = Arc::new(BodyStub(Mutex::new(String::new())));
    let indexer = Indexer::new(ProfileSet::load(None).unwrap(), stub.clone());
    let site = make_site("mteam", "https://api.m-team.cc", Some("bad-key"), None);
    let torrent = Torrent {
        site_id: site.id,
        title: "Dune.Part.Two.2024".into(),
        enclosure: "https://api.m-team.cc/api/torrent/download?id=99".into(),
        size_bytes: None,
        seeders: None,
        free: false,
        hr: None,
        imdb_id: None,
        id: Some("99".into()),
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    };

    for (envelope, expected_fragment) in [
        (
            r#"{"success":false,"message":"Invalid API Key","data":"https://api.m-team.cc/dl/leak"}"#,
            "Invalid API Key",
        ),
        (
            r#"{"code":403,"message":"Forbidden","data":"https://api.m-team.cc/dl/leak"}"#,
            "403",
        ),
        (
            r#"{"code":"1","message":"下載配額已滿","data":null}"#,
            "下載配額已滿",
        ),
    ] {
        *stub.0.lock() = envelope.into();
        let err = indexer
            .resolve_torrent_download(&site, &torrent)
            .expect_err("error envelope must be rejected even if data is populated");
        assert!(
            err.to_string().contains(expected_fragment),
            "expected '{expected_fragment}' in '{err}'"
        );
    }
}

#[test]
fn nexusphp_search_and_rss_never_attach_api_key() {
    struct InspectRequestStub;
    impl Fetcher for InspectRequestStub {
        fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
            assert_eq!(
                request.api_key, None,
                "NexusPHP request {} must never attach api_key",
                request.key
            );
            assert_eq!(request.cookie.as_deref(), Some("uid=1; pass=abc"));
            if request.key.starts_with("rss:") {
                Ok(include_str!("fixtures/rss.xml").into())
            } else {
                Ok(include_str!("fixtures/pterclub.html").into())
            }
        }
    }

    let indexer = Indexer::new(
        ProfileSet::load(None).unwrap(),
        Arc::new(InspectRequestStub),
    );
    let site = make_site(
        "pterclub",
        "https://pt.example",
        Some("stale-api-key"),
        Some("uid=1; pass=abc"),
    );

    let search_out = indexer.search(std::slice::from_ref(&site), "friends");
    assert!(search_out.failures.is_empty(), "{:?}", search_out.failures);
    assert_eq!(search_out.torrents.len(), 1);

    let rss_out = indexer.rss(std::slice::from_ref(&site));
    assert!(rss_out.failures.is_empty(), "{:?}", rss_out.failures);
    assert_eq!(rss_out.torrents.len(), 1);
}

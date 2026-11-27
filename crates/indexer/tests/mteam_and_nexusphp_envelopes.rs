use std::sync::Arc;

use domain::{Site, SiteId, Torrent};
use indexer::{FetchRequest, Fetcher, Indexer, IndexerError, RegistrySnapshot};
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
        browser_search: false,
        timezone: "UTC".into(),
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
    let indexer = Indexer::new(RegistrySnapshot::builtin().unwrap(), stub.clone());
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
    let indexer = Indexer::new(RegistrySnapshot::builtin().unwrap(), stub.clone());
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
        free_2x: false,
        free_until: None,
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
        RegistrySnapshot::builtin().unwrap(),
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

#[test]
fn mteam_and_hdarea_accept_trimmed_zero_code_and_null_nested_arrays() {
    struct BodyStub(Mutex<String>);
    impl Fetcher for BodyStub {
        fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
            Ok(self.0.lock().clone())
        }
    }

    let stub = Arc::new(BodyStub(Mutex::new(String::new())));
    let indexer = Indexer::new(RegistrySnapshot::builtin().unwrap(), stub.clone());
    let mteam = make_site("mteam", "https://api.m-team.cc", Some("ok-key"), None);
    let hdarea = make_site("hdarea", "https://hdarea.club", Some("ok-key"), None);

    for empty_mteam in [
        r#"{"code":" 0 ","message":"SUCCESS","data":{"data":null,"total":"0"}}"#,
        r#"{"code":"   ","success":true,"data":{"data":null}}"#,
    ] {
        *stub.0.lock() = empty_mteam.into();
        let out = indexer.search(std::slice::from_ref(&mteam), "dune");
        assert!(out.failures.is_empty(), "{empty_mteam}: {:?}", out.failures);
        assert!(out.torrents.is_empty());
    }

    *stub.0.lock() = r#"{"code":" 0 ","data":[{"id":"77","name":"Direct.Array.2026","imageList":[null,"https://img.m-team.cc/77.jpg"]}]}"#.into();
    let direct_out = indexer.search(std::slice::from_ref(&mteam), "direct");
    assert!(direct_out.failures.is_empty(), "{:?}", direct_out.failures);
    assert_eq!(direct_out.torrents.len(), 1);
    assert_eq!(direct_out.torrents[0].id.as_deref(), Some("77"));
    assert_eq!(
        direct_out.torrents[0].poster_url.as_deref(),
        Some("https://img.m-team.cc/77.jpg")
    );

    *stub.0.lock() =
        r#"{"code":"0","data":{"data":[{"id":"78","name":"Null.Images.2026","imageList":null}]}}"#
            .into();
    let null_img_out = indexer.search(std::slice::from_ref(&mteam), "nullimg");
    assert!(
        null_img_out.failures.is_empty(),
        "{:?}",
        null_img_out.failures
    );
    assert_eq!(null_img_out.torrents.len(), 1);
    assert_eq!(null_img_out.torrents[0].poster_url, None);

    for empty_hdarea in [
        r#"{"success":true,"code":" 0 ","data":{"items":null,"total":0}}"#,
        r#"{"success":true,"code":"   ","data":null}"#,
    ] {
        *stub.0.lock() = empty_hdarea.into();
        let out = indexer.search(std::slice::from_ref(&hdarea), "dune");
        assert!(
            out.failures.is_empty(),
            "{empty_hdarea}: {:?}",
            out.failures
        );
        assert!(out.torrents.is_empty());
    }
}

#[test]
fn hdarea_search_checks_error_envelope_before_deserializing_scalar_data() {
    struct BodyStub(Mutex<String>);
    impl Fetcher for BodyStub {
        fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
            Ok(self.0.lock().clone())
        }
    }

    let stub = Arc::new(BodyStub(Mutex::new(
        r#"{"success":false,"code":" 401 ","http_code":401,"message":"Invalid API Key","data":""}"#
            .into(),
    )));
    let indexer = Indexer::new(RegistrySnapshot::builtin().unwrap(), stub.clone());
    let hdarea = make_site("hdarea", "https://hdarea.club", Some("bad-key"), None);

    let auth_out = indexer.search(std::slice::from_ref(&hdarea), "dune");
    assert_eq!(auth_out.failures.len(), 1);
    assert!(
        auth_out.failures[0].error.contains("Invalid API Key")
            && !auth_out.failures[0].error.starts_with("parse:"),
        "expected auth fetch error instead of HdareaData parse error, got: {}",
        auth_out.failures[0].error
    );

    *stub.0.lock() =
        r#"{"success":false,"code":"429","message":"Too Many Requests","data":""}"#.into();
    let rate_out = indexer.search(std::slice::from_ref(&hdarea), "dune");
    assert_eq!(rate_out.failures.len(), 1);
    assert!(
        rate_out.failures[0].error.contains("Too Many Requests")
            && rate_out.failures[0].error.contains("429"),
        "got: {}",
        rate_out.failures[0].error
    );
}

use std::sync::Arc;

use domain::{MaintenanceVia, Site, SiteId, Torrent};
use indexer::{FetchRequest, Fetcher, Indexer, IndexerError, RegistrySnapshot};
use parking_lot::Mutex;

#[test]
fn only_source_not_found_is_a_definitive_source_rejection() {
    let missing_err = IndexerError::SourceNotFound("404 torrent deleted".into());
    assert!(missing_err.is_definitive_source_rejection());

    let yaml_err = serde_yaml::from_str::<u32>("not-a-number").unwrap_err();
    let io_err = std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout");
    let non_definitive = [
        IndexerError::BrowserBusy,
        IndexerError::BrowserSetup("cdp closed".into()),
        IndexerError::Fetch("503 service unavailable".into()),
        IndexerError::Parse("unexpected HTML gateway body".into()),
        IndexerError::UnknownProfile("custom".into()),
        IndexerError::Io(io_err),
        IndexerError::Yaml(yaml_err),
    ];

    for err in non_definitive {
        assert!(!err.is_definitive_source_rejection(), "unexpected: {err:?}");
    }
}

fn api_site(profile_id: &str, url: &str) -> (Site, Torrent) {
    let site = Site {
        id: SiteId::new(),
        name: profile_id.into(),
        url: url.into(),
        profile_id: profile_id.into(),
        cookie: None,
        api_key: Some("secret-api-key".into()),
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: MaintenanceVia::Http,
        browser_search: false,
        timezone: "UTC".into(),
    };
    let torrent = Torrent {
        site_id: site.id,
        title: "The.Matrix.1999.2160p".into(),
        enclosure: format!("{}/download?id=42", url.trim_end_matches('/')),
        size_bytes: None,
        seeders: None,
        free: false,
        hr: None,
        imdb_id: None,
        id: Some("42".into()),
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    };
    (site, torrent)
}

#[test]
fn mteam_and_hdarea_download_resolution_only_marks_missing_sources_definitive() {
    struct BodyStub(Mutex<String>);
    impl Fetcher for BodyStub {
        fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
            Ok(self.0.lock().clone())
        }
    }

    let stub = Arc::new(BodyStub(Mutex::new(String::new())));
    let indexer = Indexer::new(RegistrySnapshot::builtin().unwrap(), stub.clone());
    let (mteam, mteam_torrent) = api_site("mteam", "https://api.m-team.cc");
    let (hdarea, hdarea_torrent) = api_site("hdarea", "https://hdarea.club");

    for body in [
        r#"{"code":"404","message":"种子不存在","data":null}"#,
        r#"{"code":410,"message":"已删除","data":null}"#,
        r#"{"code":"0","message":"SUCCESS","data":""}"#,
        r#"{"code":"0","message":"SUCCESS","data":null}"#,
        r#"{"success":true,"data":"   "}"#,
    ] {
        *stub.0.lock() = body.into();
        let err = indexer
            .resolve_torrent_download(&mteam, &mteam_torrent)
            .unwrap_err();
        assert!(
            err.is_definitive_source_rejection(),
            "expected SourceNotFound for M-Team {body}, got: {err:?}"
        );
    }

    for body in [
        r#"{"code":"429","message":"請求過於頻繁","data":null}"#,
        r#"{"code":"401","message":"Invalid API Key","data":null}"#,
        r#"{"code":403,"message":"Forbidden","data":null}"#,
        r#"{"code":"502","message":"Bad Gateway","data":null}"#,
        r#"{"code":"1","message":"下載配額已滿","data":null}"#,
        r#"{"error":"upstream gateway timeout"}"#,
        "<!DOCTYPE html><html><head><title>Just a moment...</title></head></html>",
    ] {
        *stub.0.lock() = body.into();
        let err = indexer
            .resolve_torrent_download(&mteam, &mteam_torrent)
            .unwrap_err();
        assert!(
            !err.is_definitive_source_rejection(),
            "M-Team transient/auth/quota/HTML must NOT be definitive: {body} -> {err:?}"
        );
    }

    for body in [
        r#"{"success":false,"code":"404","http_code":404,"message":"Not Found"}"#,
        r#"{"success":false,"http_code":410,"message":"Gone"}"#,
        r#"{"success":true,"code":0,"data":null}"#,
        r#"{"success":true,"code":"0","data":{"download_url":"  "}}"#,
    ] {
        *stub.0.lock() = body.into();
        let err = indexer
            .resolve_torrent_download(&hdarea, &hdarea_torrent)
            .unwrap_err();
        assert!(
            err.is_definitive_source_rejection(),
            "expected SourceNotFound for HDArea {body}, got: {err:?}"
        );
    }

    for body in [
        r#"{"success":false,"code":401,"http_code":401,"message":"Invalid API Key"}"#,
        r#"{"success":false,"code":"403","http_code":403,"message":"Forbidden"}"#,
        r#"{"success":false,"code":"429","http_code":429,"message":"Too Many Requests"}"#,
        r#"{"success":false,"code":500,"http_code":500,"message":"DB overloaded"}"#,
        r#"{"success":false,"code":"1","message":"今日下载配额已用尽"}"#,
        r#"{"error":"gateway timeout"}"#,
        "<html><body>502 Bad Gateway</body></html>",
    ] {
        *stub.0.lock() = body.into();
        let err = indexer
            .resolve_torrent_download(&hdarea, &hdarea_torrent)
            .unwrap_err();
        assert!(
            !err.is_definitive_source_rejection(),
            "HDArea transient/auth/quota/HTML must NOT be definitive: {body} -> {err:?}"
        );
    }

    let mut missing_id_torrent = mteam_torrent;
    missing_id_torrent.id = None;
    missing_id_torrent.enclosure = "https://api.m-team.cc/mt.torrent".into();
    let missing_id_err = indexer
        .resolve_torrent_download(&mteam, &missing_id_torrent)
        .unwrap_err();
    assert!(matches!(missing_id_err, IndexerError::InvalidInput(_)));
}

use domain::{DownloaderId, MaintenanceVia, Site, SiteId};
use store::{DownloaderRow, Store};

fn site() -> Site {
    Site {
        id: SiteId::new(),
        name: "Current".into(),
        url: "https://fixture.invalid/".into(),
        profile_id: "pterclub".into(),
        cookie: Some("original".into()),
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: MaintenanceVia::Http,
    }
}
fn downloader() -> DownloaderRow {
    DownloaderRow {
        id: DownloaderId::new(),
        name: "Current".into(),
        kind: "qbittorrent".into(),
        url: "http://fixture.invalid/".into(),
        username: None,
        password: None,
        category: None,
        path_maps: vec![],
        is_default: false,
        enabled: true,
    }
}

#[test]
fn stale_site_maintenance_cannot_overwrite_another_connection_and_current_result_persists() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let expected = site();
    store.insert_site(&expected).unwrap();
    let other = Store::open(tmp.path()).unwrap();
    let mut changed = other.get_site(expected.id).unwrap().unwrap();
    changed.url = "https://new.invalid/".into();
    other.save_site(&changed).unwrap();
    let mut stale = expected.clone();
    stale.cookie = Some("stale response".into());
    assert!(!store.save_site_if_current(&stale, &expected).unwrap());
    assert_eq!(other.get_site(expected.id).unwrap().unwrap(), changed);
    let mut renewed = changed.clone();
    renewed.cookie = Some("current response".into());
    assert!(store.save_site_if_current(&renewed, &changed).unwrap());
    drop(store);
    drop(other);
    assert_eq!(
        Store::open(tmp.path())
            .unwrap()
            .get_site(expected.id)
            .unwrap()
            .unwrap(),
        renewed
    );
}

#[test]
fn verification_result_matches_exact_config_and_status_pair_survives_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let expected = downloader();
    store.insert_downloader(&expected).unwrap();
    assert!(
        store
            .record_downloader_verification(&expected, Ok(()))
            .unwrap()
    );
    let key = format!("downloader.{}.verified", expected.id);
    let error_key = format!("downloader.{}.last_error", expected.id);
    let other = Store::open(tmp.path()).unwrap();
    let mut changed = expected.clone();
    changed.url = "http://changed.invalid/".into();
    other.save_downloader(&changed).unwrap();
    assert!(
        !store
            .record_downloader_verification(&expected, Err("stale failure"))
            .unwrap()
    );
    assert_eq!(other.get_setting(&key).unwrap().as_deref(), Some("true"));
    assert!(other.get_setting(&error_key).unwrap().is_none());
    assert!(
        store
            .record_downloader_verification(&changed, Err("current failure"))
            .unwrap()
    );
    drop(store);
    drop(other);
    let reopened = Store::open(tmp.path()).unwrap();
    assert_eq!(
        reopened.get_setting(&key).unwrap().as_deref(),
        Some("false")
    );
    assert_eq!(
        reopened.get_setting(&error_key).unwrap().as_deref(),
        Some("current failure")
    );
}

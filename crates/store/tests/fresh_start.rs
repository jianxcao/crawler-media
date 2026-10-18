use rusqlite::Connection;
use store::Store;
use tempfile::tempdir;

#[test]
fn empty_data_directory_initializes_the_current_schema() {
    let dir = tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let sites = store.list_sites().unwrap();
    assert!(sites.is_empty());

    // Verify sites table has maintenance_via column defaulting to 'http'
    let site = domain::Site {
        id: domain::SiteId::new(),
        name: "test".into(),
        url: "https://example.com/".into(),
        profile_id: "demo".into(),
        cookie: None,
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Http,
    };
    store.insert_site(&site).unwrap();
    let loaded = store.get_site(site.id).unwrap().unwrap();
    assert_eq!(loaded.maintenance_via, domain::MaintenanceVia::Http);
}

#[test]
fn legacy_v7_database_is_rejected_as_invalid_layout() {
    let dir = tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let app_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&app_path).unwrap();
        conn.pragma_update(None, "user_version", 7).unwrap();
    }

    let err = Store::open(dir.path()).expect_err("v7 database must be rejected as legacy layout");
    match err {
        store::StoreError::Invalid(msg) => {
            assert!(msg.contains("app database layout mismatch"));
        }
        other => panic!("expected StoreError::Invalid, got {other:?}"),
    }
}

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

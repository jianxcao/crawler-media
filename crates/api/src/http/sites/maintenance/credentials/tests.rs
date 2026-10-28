use super::*;
use api_fixture::*;
use std::sync::Arc;

mod api_fixture {
    use super::*;
    struct NoFetch;
    impl indexer::Fetcher for NoFetch {
        fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
            Err(indexer::IndexerError::Fetch("offline fixture".into()))
        }
    }
    pub fn fixture() -> (tempfile::TempDir, ApiState, Site) {
        let tmp = tempfile::tempdir().unwrap();
        let state = ApiState::new(
            crate::Store::open(tmp.path().join("data")).unwrap(),
            "credential-secret".into(),
            indexer::ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        )
        .unwrap();
        let site = Site {
            id: SiteId::new(),
            name: "Current".into(),
            url: "https://fixture.invalid".into(),
            profile_id: "pterclub".into(),
            cookie: Some("original".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Http,
        };
        state.store().lock().insert_site(&site).unwrap();
        (tmp, state, site)
    }
}

#[test]
fn credential_save_rechecks_readonly_and_maintenance_after_load() {
    for maintenance in [false, true] {
        let (_tmp, state, site) = fixture();
        let creds = AdmittedCredentials::new(&state, site.clone());
        let mut renewed = creds.load(site.id).unwrap();
        renewed.cookie = Some("renewed".into());
        let gate = state.write_gate();
        let closed = if maintenance {
            Some(gate.close())
        } else {
            let _ = state.clone().with_instance_writable(false);
            None
        };
        assert!(creds.save(&renewed).is_err());
        let response = creds.take_rejection().unwrap();
        assert_eq!(
            response.status().as_u16(),
            if maintenance { 503 } else { 409 }
        );
        assert_eq!(
            state
                .store()
                .lock()
                .get_site(site.id)
                .unwrap()
                .unwrap()
                .cookie,
            site.cookie
        );
        drop(closed);
    }
}

#[test]
fn credential_save_does_not_overwrite_a_concurrent_site_update() {
    let (_tmp, state, site) = fixture();
    let creds = AdmittedCredentials::new(&state, site.clone());
    let mut renewed = creds.load(site.id).unwrap();
    renewed.cookie = Some("renewed".into());
    let mut changed = site.clone();
    changed.url = "https://changed.invalid".into();
    state.store().lock().save_site(&changed).unwrap();
    assert!(creds.save(&renewed).is_err());
    assert!(state.store().lock().get_site(site.id).unwrap().unwrap() == changed);
}

#[test]
fn credential_save_publishes_current_cookie() {
    let (_tmp, state, site) = fixture();
    let creds = AdmittedCredentials::new(&state, site.clone());
    let mut renewed = creds.load(site.id).unwrap();
    renewed.cookie = Some("renewed".into());
    creds.save(&renewed).unwrap();
    assert!(creds.take_rejection().is_none());
    assert!(state.store().lock().get_site(site.id).unwrap().unwrap() == renewed);
}

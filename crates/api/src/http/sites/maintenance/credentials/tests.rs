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

fn queued_credential_save(maintenance: bool) {
    use std::sync::mpsc;
    use std::time::Duration;

    let (_tmp, state, site) = fixture();
    let gate = state.write_gate();
    let writers = state.store().lock().writers();
    let renewed = Site {
        cookie: Some("renewed".into()),
        ..site.clone()
    };
    let (entered_tx, entered_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    let (worker, closed) = writers
        .with_writer(store::Db::App, |_| {
            let state_for_save = state.clone();
            let expected = site.clone();
            let candidate = renewed.clone();
            let worker = std::thread::spawn(move || {
                let creds = AdmittedCredentials::new(&state_for_save, expected);
                entered_tx.send(()).unwrap();
                let rejected = creds.save(&candidate).is_err();
                let status = creds
                    .take_rejection()
                    .map(|response| response.status().as_u16());
                finished_tx.send((rejected, status)).unwrap();
            });
            entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            assert!(matches!(
                finished_rx.recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            let closed = if maintenance {
                Some(gate.close())
            } else {
                let _ = state.clone().with_instance_writable(false);
                None
            };
            Ok((worker, closed))
        })
        .unwrap();
    worker.join().unwrap();
    let (rejected, status) = finished_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(
        rejected,
        "queued Site credentials must not publish after admission closes"
    );
    assert_eq!(status, Some(if maintenance { 503 } else { 409 }));
    assert!(state.store().lock().get_site(site.id).unwrap().unwrap() == site);
    drop(closed);
    let _ = state.clone().with_instance_writable(true);
    let creds = AdmittedCredentials::new(&state, site);
    creds.save(&renewed).unwrap();
    assert!(creds.take_rejection().is_none());
    assert!(state.store().lock().get_site(renewed.id).unwrap().unwrap() == renewed);
}

#[test]
fn queued_credential_save_rechecks_maintenance_at_app_writer() {
    queued_credential_save(true);
}

#[test]
fn queued_credential_save_rechecks_instance_at_app_writer() {
    queued_credential_save(false);
}

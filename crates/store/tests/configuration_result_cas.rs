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
        api_key: None,
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

#[test]
fn downloader_api_key_persists_lists_clears_and_triggers_verification_cas() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let mut row = downloader();
    row.is_default = true;
    row.api_key = Some("qbt_first_key".into());
    store.insert_downloader(&row).unwrap();

    assert_eq!(
        store
            .get_downloader(row.id)
            .unwrap()
            .unwrap()
            .api_key
            .as_deref(),
        Some("qbt_first_key")
    );
    assert_eq!(
        store
            .default_downloader()
            .unwrap()
            .unwrap()
            .api_key
            .as_deref(),
        Some("qbt_first_key")
    );
    assert_eq!(
        store.list_downloaders().unwrap()[0].api_key.as_deref(),
        Some("qbt_first_key")
    );

    // Changing api_key concurrently invalidates stale verification CAS.
    let other = Store::open(tmp.path()).unwrap();
    let mut updated = row.clone();
    updated.api_key = Some("qbt_rotated_key".into());
    other.save_downloader(&updated).unwrap();
    assert!(
        !store.record_downloader_verification(&row, Ok(())).unwrap(),
        "changed api_key must trigger CAS conflict"
    );
    assert!(
        store
            .record_downloader_verification(&updated, Ok(()))
            .unwrap()
    );

    // Clearing api_key deletes the setting cleanly.
    updated.api_key = None;
    store.save_downloader(&updated).unwrap();
    assert!(
        store
            .get_downloader(row.id)
            .unwrap()
            .unwrap()
            .api_key
            .is_none()
    );

    // Re-set then delete downloader cleans up setting as well.
    updated.api_key = Some("qbt_temp_key".into());
    store.save_downloader(&updated).unwrap();
    assert!(store.delete_downloader(row.id).unwrap());
    assert!(
        store
            .get_setting(&format!("downloader.{}.api_key", row.id))
            .unwrap()
            .is_none()
    );
}

#[test]
fn mutate_setting_updates_and_deletes_atomically_in_one_writer_tx() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    // Protected Library keys must be rejected before reading/writing.
    assert!(matches!(
        store.mutate_setting("metadata.language", |cur| Ok((cur, ()))),
        Err(store::StoreError::Invalid(_))
    ));

    // Concurrent calls run serialized inside one writer tx so no key is lost.
    let store = std::sync::Arc::new(store);
    let s1 = store.clone();
    let s2 = store.clone();
    let t1 = std::thread::spawn(move || {
        s1.mutate_setting("user.test.up_next.dismissed", |cur: Option<String>| {
            let mut items: Vec<String> = cur
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            items.push("show-a".into());
            Ok((Some(serde_json::to_string(&items).unwrap()), ()))
        })
        .unwrap();
    });
    let t2 = std::thread::spawn(move || {
        s2.mutate_setting("user.test.up_next.dismissed", |cur: Option<String>| {
            let mut items: Vec<String> = cur
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            items.push("show-b".into());
            Ok((Some(serde_json::to_string(&items).unwrap()), ()))
        })
        .unwrap();
    });
    t1.join().unwrap();
    t2.join().unwrap();

    let raw = store
        .get_setting("user.test.up_next.dismissed")
        .unwrap()
        .unwrap();
    let mut items: Vec<String> = serde_json::from_str(&raw).unwrap();
    items.sort();
    assert_eq!(items, vec!["show-a", "show-b"]);

    // Returning None deletes the row cleanly.
    store
        .mutate_setting("user.test.up_next.dismissed", |_| Ok((None, ())))
        .unwrap();
    assert!(
        store
            .get_setting("user.test.up_next.dismissed")
            .unwrap()
            .is_none()
    );
}

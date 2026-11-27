use domain::{DownloaderId, MaintenanceVia, Site, SiteId};
use store::{DownloaderRow, ManualCredentialWrite, Store};

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
        browser_search: false,
        timezone: "UTC".into(),
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
fn stale_site_maintenance_rejects_aba() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let original = site();
    store.insert_site(&original).unwrap();
    let observed = store.observe_site(original.id).unwrap().unwrap();
    let mut changed = original.clone();
    changed.cookie = Some("replacement".into());
    store
        .save_site(&changed, ManualCredentialWrite::Replace)
        .unwrap();
    store
        .save_site(&original, ManualCredentialWrite::Replace)
        .unwrap();
    let mut stale = original.clone();
    stale.cookie = Some("stale response".into());
    assert!(
        !store
            .save_site_if_current(&observed, stale.cookie.as_deref())
            .unwrap()
    );
    assert_eq!(store.get_site(original.id).unwrap(), Some(original));
}

#[test]
fn stale_site_maintenance_rejects_recreated_site() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let original = site();
    store.insert_site(&original).unwrap();
    let observed = store.observe_site(original.id).unwrap().unwrap();
    store.delete_site(original.id).unwrap();
    store.insert_site(&original).unwrap();
    assert_ne!(
        observed.credential_epoch(),
        store
            .observe_site(original.id)
            .unwrap()
            .unwrap()
            .credential_epoch()
    );
    let mut stale = original.clone();
    stale.cookie = Some("stale response".into());
    assert!(
        !store
            .save_site_if_current(&observed, stale.cookie.as_deref())
            .unwrap()
    );
    assert_eq!(store.get_site(original.id).unwrap(), Some(original));
}

#[test]
fn site_observations_track_credential_authority_and_revisions() {
    let tmp = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(Store::open(tmp.path()).unwrap());
    let mut current = site();
    store.insert_site(&current).unwrap();
    let first = store.observe_site(current.id).unwrap().unwrap();
    let reopened = Store::open(tmp.path()).unwrap();
    assert_eq!(reopened.observe_site(current.id).unwrap().unwrap(), first);
    for field in 0..4 {
        match field {
            0 => current.cookie = Some("new".into()),
            1 => current.api_key = Some("key".into()),
            2 => current.rss_url = Some("rss".into()),
            _ => (),
        }
        let before = store.observe_site(current.id).unwrap().unwrap();
        store
            .save_site(&current, ManualCredentialWrite::Replace)
            .unwrap();
        let after = reopened.observe_site(current.id).unwrap().unwrap();
        assert_ne!(before.credential_epoch(), after.credential_epoch());
        assert!(after.revision() > before.revision());
    }
    let before = store.observe_site(current.id).unwrap().unwrap();
    current.url = "https://new.invalid".into();
    store
        .save_site(&current, ManualCredentialWrite::Unchanged)
        .unwrap();
    let after = store.observe_site(current.id).unwrap().unwrap();
    assert_ne!(before.credential_epoch(), after.credential_epoch());
    let peer = store.clone().observe_site(current.id).unwrap().unwrap();
    assert!(store.save_site_if_current(&after, Some("renewed")).unwrap());
    assert!(!reopened.save_site_if_current(&peer, Some("stale")).unwrap());
    let refreshed = store.observe_site(current.id).unwrap().unwrap();
    assert_eq!(after.credential_epoch(), refreshed.credential_epoch());
    current = refreshed.site().clone();
    current.name = "renamed".into();
    store
        .save_site(&current, ManualCredentialWrite::Unchanged)
        .unwrap();
    let renamed = store.observe_site(current.id).unwrap().unwrap();
    assert_eq!(refreshed.credential_epoch(), renamed.credential_epoch());
    assert!(
        !store
            .save_site_if_current(&refreshed, Some("stale"))
            .unwrap()
    );
    store.set_site_enabled(current.id, false).unwrap();
    let disabled = store.observe_site(current.id).unwrap().unwrap();
    assert_eq!(renamed.credential_epoch(), disabled.credential_epoch());
    assert!(!store.save_site_if_current(&renamed, Some("stale")).unwrap());
}

#[test]
fn unchanged_credential_intent_rejects_all_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let current = site();
    store.insert_site(&current).unwrap();
    let before = store.observe_site(current.id).unwrap().unwrap();
    for field in 0..3 {
        let mut changed = current.clone();
        changed.name = "changed".into();
        match field {
            0 => changed.cookie = Some("changed".into()),
            1 => changed.api_key = Some("changed".into()),
            _ => changed.rss_url = Some("changed".into()),
        }
        assert!(
            store
                .save_site(&changed, ManualCredentialWrite::Unchanged)
                .is_err()
        );
        assert_eq!(store.observe_site(current.id).unwrap().unwrap(), before);
    }
}

#[test]
fn app_write_enforces_site_authority_without_nested_transactions() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let mut current = site();
    store.app_write(|app| app.insert_site(&current)).unwrap();
    let before = store.observe_site(current.id).unwrap().unwrap();
    current.cookie = Some("manual".into());
    assert!(
        store
            .app_write(|app| app.save_site(&current, ManualCredentialWrite::Unchanged))
            .is_err()
    );
    assert_eq!(store.observe_site(current.id).unwrap().unwrap(), before);
    store
        .app_write(|app| app.save_site(&current, ManualCredentialWrite::Replace))
        .unwrap();
    assert!(
        !store
            .app_write(|app| app.save_site_if_current(&before, Some("stale")))
            .unwrap()
    );
    let after = store.observe_site(current.id).unwrap().unwrap();
    assert_ne!(before.credential_epoch(), after.credential_epoch());
}

#[test]
fn stale_site_maintenance_cannot_overwrite_another_connection_and_current_result_persists() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let expected = site();
    store.insert_site(&expected).unwrap();
    let observed = store.observe_site(expected.id).unwrap().unwrap();
    let other = Store::open(tmp.path()).unwrap();
    let mut changed = other.get_site(expected.id).unwrap().unwrap();
    changed.url = "https://new.invalid/".into();
    other
        .save_site(&changed, ManualCredentialWrite::Unchanged)
        .unwrap();
    let mut stale = expected.clone();
    stale.cookie = Some("stale response".into());
    assert!(
        !store
            .save_site_if_current(&observed, stale.cookie.as_deref())
            .unwrap()
    );
    assert_eq!(other.get_site(expected.id).unwrap().unwrap(), changed);
    let mut renewed = changed.clone();
    renewed.cookie = Some("current response".into());
    let observed = other.observe_site(changed.id).unwrap().unwrap();
    assert!(
        store
            .save_site_if_current(&observed, renewed.cookie.as_deref())
            .unwrap()
    );
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

#[test]
fn sites_browser_search_and_timezone_roundtrip_and_preserve_cas() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let mut current = site();
    current.browser_search = true;
    current.timezone = "Asia/Shanghai".into();
    store.insert_site(&current).unwrap();

    let loaded = store.get_site(current.id).unwrap().unwrap();
    assert!(loaded.browser_search);
    assert_eq!(loaded.timezone, "Asia/Shanghai");

    let obs1 = store.observe_site(current.id).unwrap().unwrap();
    assert!(
        store
            .save_site_if_current(&obs1, Some("refreshed"))
            .unwrap()
    );
    let after_cookie = store.observe_site(current.id).unwrap().unwrap();
    assert_eq!(after_cookie.credential_epoch(), obs1.credential_epoch());
    assert!(after_cookie.site().browser_search);
    assert_eq!(after_cookie.site().timezone, "Asia/Shanghai");

    let mut updated = after_cookie.site().clone();
    updated.browser_search = false;
    updated.timezone = "America/New_York".into();
    store
        .save_site(&updated, ManualCredentialWrite::Unchanged)
        .unwrap();
    let after_optin = store.observe_site(current.id).unwrap().unwrap();
    assert_eq!(after_optin.credential_epoch(), obs1.credential_epoch());
    assert!(!after_optin.site().browser_search);
    assert_eq!(after_optin.site().timezone, "America/New_York");
    assert!(
        !store
            .save_site_if_current(&after_cookie, Some("stale"))
            .unwrap()
    );
}

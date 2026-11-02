//! Current instance configuration has one revision and no reads from retired keys.
use store::{Store, StoreError, TransferSettingsPatch};

#[test]
fn concurrent_editors_cannot_overwrite_a_newer_transfer_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let first = Store::open(dir.path()).unwrap();
    let second = Store::open(dir.path()).unwrap();
    let revision = first.transfer_settings().unwrap().revision;
    let saved = first
        .patch_transfer_settings(
            revision,
            TransferSettingsPatch {
                transfer_mode: Some("copy".into()),
                watch_intake: Some(Some(" /incoming ".into())),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(matches!(
        second.patch_transfer_settings(
            revision,
            TransferSettingsPatch {
                scrape: Some(true),
                ..Default::default()
            }
        ),
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(second.transfer_settings().unwrap(), saved);
    second
        .patch_transfer_settings(
            saved.revision,
            TransferSettingsPatch {
                watch_intake: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(first.watch_intake().unwrap(), None);
    assert_eq!(first.transfer_mode().unwrap(), "copy");
    assert!(!first.scrape_enabled().unwrap());
}

#[test]
fn retired_scalar_keys_cannot_configure_transfer_or_watch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    for (key, value) in [
        ("transfer_mode", "move"),
        ("scrape", "true"),
        ("watch_intake", "/old"),
        ("watch_inplace", "/old"),
    ] {
        store.put_setting(key, value).unwrap();
    }
    assert_eq!(store.transfer_settings().unwrap(), Default::default());
}

#[test]
fn library_configuration_persists_and_app_cannot_bypass_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.set_transfer_mode("move").unwrap();
    store
        .save_raw_scrape_settings("broken json retained")
        .unwrap();
    let scrape = store.raw_scrape_settings().unwrap();
    for key in ["transfer.settings", "metadata.scrape"] {
        assert!(matches!(
            store.put_setting(key, "{}"),
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.delete_setting(key),
            Err(StoreError::Invalid(_))
        ));
    }
    drop(store);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.transfer_mode().unwrap(), "move");
    assert_eq!(reopened.raw_scrape_settings().unwrap(), scrape);
    reopened.save_raw_scrape_settings("{}").unwrap();
    reopened
        .save_raw_scrape_settings("broken json retained")
        .unwrap();
    assert_eq!(
        reopened.raw_scrape_settings().unwrap().revision,
        scrape.revision + 2
    );
}

#[test]
fn library_snapshot_keeps_transfer_and_scrape_configuration_coherent() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.save_raw_scrape_settings("before").unwrap();
    let initial = store.raw_scrape_settings().unwrap();
    store
        .library_read_snapshot(|db| {
            assert_eq!(db.transfer_settings().unwrap().revision, 0);
            std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        store.set_transfer_mode("copy").unwrap();
                        store.save_raw_scrape_settings("after").unwrap();
                    })
                    .join()
                    .unwrap();
            });
            assert_eq!(db.transfer_settings().unwrap().revision, 0);
            assert_eq!(db.raw_scrape_settings().unwrap(), initial);
        })
        .unwrap();
    assert_eq!(store.transfer_mode().unwrap(), "copy");
    assert_eq!(
        store.raw_scrape_settings().unwrap().value.as_deref(),
        Some("after")
    );
}

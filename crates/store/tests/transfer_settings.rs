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

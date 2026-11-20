use super::*;

#[test]
fn startup_discards_settings_snapshot_after_a_newer_publication() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            marker::media_read::set_remote_reads_paused(false);
        }
    }
    let _reset = Reset;
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(tmp.path()).unwrap());
    let manager = ProbeManager::new(store).unwrap();
    let observed = manager.settings_revision();
    manager.apply_remote_reads_pause(true);
    assert!(!manager.apply_startup_settings_if_unchanged(
        observed,
        Some(crate::probe_settings::ProbeSettings {
            metadata_workers: 1,
            remote_concurrency: 1,
            remote_cooldown_ms: 500,
        }),
        Some(crate::probe_settings::RemoteReadsSettings { paused: false }),
    ));
    assert!(manager.remote_reads_are_paused());
    assert!(marker::media_read::remote_reads_paused());
}

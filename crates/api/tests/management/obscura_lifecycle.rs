use api::obscura_manager::ObscuraManager;
use tempfile::tempdir;

#[test]
fn obscura_manager_paths_and_initial_status() {
    let tmp = tempdir().unwrap();
    let manager = ObscuraManager::new(tmp.path());

    assert_eq!(manager.cdp_url(), "http://127.0.0.1:9223");
    assert!(!manager.is_installed());
    assert!(!manager.is_downloading());
    assert_eq!(
        manager.binary_path(),
        tmp.path().join("bin").join(if cfg!(windows) {
            "obscura.exe"
        } else {
            "obscura"
        })
    );
}

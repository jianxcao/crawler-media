use api::{ChosenDownloader, DownloaderEnv, Store, choose_downloader};

fn empty_env() -> DownloaderEnv {
    DownloaderEnv {
        qb_url: None,
        qb_user: None,
        qb_pass: None,
        qb_category: None,
        qb_path_maps: vec![],
        tr_path_maps: vec![],
    }
}

fn qb_setting() -> String {
    serde_json::json!({
        "name": "nas",
        "kind": "qbittorrent",
        "url": "http://qb.example:8080",
        "username": "admin",
        "password": "secret",
        "is_default": true
    })
    .to_string()
}

#[test]
fn old_settings_cannot_select_a_downloader() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.put_setting("downloader", &qb_setting()).unwrap();

    assert!(choose_downloader(&store, &empty_env()).is_err());
}

#[test]
fn env_overrides_saved_downloader() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.put_setting("downloader", &qb_setting()).unwrap();
    let env = DownloaderEnv {
        qb_url: Some("http://env-qb:8080".into()),
        qb_user: Some("env-user".into()),
        qb_pass: Some("env-pass".into()),
        qb_category: Some("media".into()),
        qb_path_maps: vec![],
        tr_path_maps: vec![],
    };

    let ChosenDownloader::Qbittorrent(cfg) = choose_downloader(&store, &env).unwrap() else {
        panic!("expected env qBittorrent Downloader");
    };
    assert_eq!(cfg.url, "http://env-qb:8080");
    assert_eq!(cfg.username, "env-user");
    assert_eq!(cfg.password, "env-pass");
    assert_eq!(cfg.category, Some("media".into()));
}

#[test]
fn missing_downloader_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert!(choose_downloader(&store, &empty_env()).is_err());
}

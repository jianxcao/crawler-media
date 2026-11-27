use api::{DownloaderEnv, DynamicDownloader, Store};
use domain::{DownloaderId, SiteId, Torrent};
use downloader::Downloader;
use std::sync::Arc;

fn torrent() -> Torrent {
    Torrent {
        site_id: SiteId::new(),
        title: "Film.2020.1080p".into(),
        enclosure: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
        size_bytes: None,
        seeders: None,
        free: true,
        hr: Some(false),
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
        free_2x: false,
        free_until: None,
    }
}

fn assert_submission_rejected(kind: Option<&str>, enabled: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    if let Some(kind) = kind {
        store
            .insert_downloader(&store::DownloaderRow {
                id: DownloaderId::new(),
                name: "invalid-production-config".into(),
                kind: kind.into(),
                url: "http://127.0.0.1:1".into(),
                username: Some("admin".into()),
                password: None,
                api_key: None,
                category: None,
                path_maps: vec![],
                is_default: true,
                enabled,
            })
            .unwrap();
    }
    let downloader = DynamicDownloader::new(Arc::new(store), DownloaderEnv::default(), tmp.path());
    let _ = torrent();
    let payload = downloader::SubmitPayload::Magnet {
        uri: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
        info_hash: "0123456789abcdef0123456789abcdef01234567".into(),
    };
    assert!(
        downloader
            .submit_verified(&payload, &downloader::SubmitOptions::default())
            .is_err(),
        "production cannot accept a fake in-memory submission"
    );
}

#[test]
fn incomplete_default_does_not_accept_fake_download() {
    assert_submission_rejected(Some("qbittorrent"), true);
}
#[test]
fn absent_downloader_does_not_accept_fake_download() {
    assert_submission_rejected(None, true);
}
#[test]
fn disabled_default_does_not_accept_fake_download() {
    assert_submission_rejected(Some("qbittorrent"), false);
}
#[test]
fn unknown_default_kind_does_not_accept_fake_download() {
    assert_submission_rejected(Some("unsupported"), true);
}

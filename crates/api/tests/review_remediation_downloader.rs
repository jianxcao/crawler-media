use std::sync::Arc;
use parking_lot::Mutex;
use api::{Store, DynamicDownloader, DownloaderEnv};
use domain::{DownloaderId, SiteId, Torrent};
use downloader::Downloader;

fn torrent() -> Torrent {
    Torrent {
        site_id: SiteId::new(), title: "Film.2020.1080p".into(),
        enclosure: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
        size_bytes: None, seeders: None, free: true, hr: false, imdb_id: None,
        id: None, leechers: None, snatched: None, upload_time: None,
        detail_url: None, category: None, poster_url: None,
    }
}

fn assert_submission_rejected(kind: Option<&str>, enabled: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    if let Some(kind) = kind {
        store.insert_downloader(&store::DownloaderRow {
            id: DownloaderId::new(), name: "invalid-production-config".into(), kind: kind.into(),
            url: "http://127.0.0.1:1".into(), username: Some("admin".into()), password: None,
            category: None, path_maps: vec![], is_default: true, enabled,
        }).unwrap();
    }
    let downloader = DynamicDownloader::new(Arc::new(Mutex::new(store)), DownloaderEnv::default(), tmp.path());
    assert!(downloader.add(&torrent()).is_err(), "production cannot accept a fake in-memory submission");
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

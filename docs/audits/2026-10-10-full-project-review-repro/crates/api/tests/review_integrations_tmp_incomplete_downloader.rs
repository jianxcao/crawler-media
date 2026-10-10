use std::sync::Arc;
use parking_lot::Mutex;
use api::{Store, DynamicDownloader, DownloaderEnv};
use domain::{DownloaderId, SiteId, Torrent};
use downloader::Downloader;

#[test]
fn incomplete_default_must_not_accept_submission_through_memory_downloader() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    store.insert_downloader(&store::DownloaderRow {
        id: DownloaderId::new(), name: "incomplete-qbit".into(), kind: "qbittorrent".into(),
        url: "http://127.0.0.1:1".into(), username: Some("admin".into()), password: None,
        category: None, path_maps: vec![], is_default: true, enabled: true,
    }).unwrap();
    let downloader = DynamicDownloader::new(Arc::new(Mutex::new(store)), DownloaderEnv::default(), tmp.path());
    let torrent = Torrent {
        site_id: SiteId::new(), title: "Film.2020.1080p".into(),
        enclosure: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
        size_bytes: None, seeders: None, free: true, hr: false, imdb_id: None,
        id: None, leechers: None, snatched: None, upload_time: None,
        detail_url: None, category: None, poster_url: None,
    };
    assert!(downloader.add(&torrent).is_err(), "incomplete production config accepted a fake download: endpoint={:?}", downloader.endpoint());
}

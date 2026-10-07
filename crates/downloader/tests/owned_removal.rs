use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use domain::{SiteId, Torrent};
use downloader::{
    Downloader, QbitConfig, QbitDownloader, TransmissionConfig, TransmissionDownloader,
};
use serde_json::json;

fn torrent() -> Torrent {
    torrent_with("https://pt.example/original")
}

fn torrent_with(enclosure: &str) -> Torrent {
    Torrent {
        site_id: SiteId::new(),
        title: "Identical.Release.1080p".into(),
        enclosure: enclosure.into(),
        size_bytes: Some(100),
        seeders: None,
        free: false,
        hr: false,
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    }
}

fn unrelated_qbit_row() -> serde_json::Value {
    json!([{
        "hash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "name": "Identical.Release.1080p",
        "size": 100,
        "progress": 1,
        "save_path": "/downloads",
        "tags": "crawler-media"
    }])
}

fn unrelated_transmission_row() -> serde_json::Value {
    json!([{
        "id": 9,
        "hashString": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "name": "Identical.Release.1080p",
        "sizeWhenDone": 100,
        "labels": ["crawler-media"]
    }])
}

fn fixture(transmission: bool, rows: serde_json::Value) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut stream = incoming.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..n]);
                if request_complete(&bytes) {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&bytes).into_owned();
            let body = if transmission {
                if request.contains("session-get") {
                    json!({"result":"success","arguments":{"rpc-version":16}})
                } else {
                    json!({"result":"success","arguments":{"torrents":rows}})
                }
                .to_string()
            } else if request.contains("auth/login") {
                "Ok.".into()
            } else if request.contains("torrents/info") {
                rows.to_string()
            } else {
                "Ok.".into()
            };
            captured.lock().unwrap().push(request);
            write!(stream, "HTTP/1.1 200 OK\r\nSet-Cookie: SID=fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (url, requests)
}

fn request_complete(bytes: &[u8]) -> bool {
    let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..end]);
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    bytes.len() >= end + 4 + length
}

#[test]
fn qbit_original_absent_identical_unrelated_release_is_never_deleted() {
    let (url, requests) = fixture(false, unrelated_qbit_row());
    let dl = QbitDownloader::connect(QbitConfig {
        url,
        username: "u".into(),
        password: "p".into(),
        category: None,
        path_maps: vec![],
    })
    .unwrap();
    assert!(dl.remove_owned(&torrent(), true).is_err());
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.contains("torrents/delete"))
    );
}

#[test]
fn transmission_original_absent_identical_unrelated_release_is_never_deleted() {
    let (url, requests) = fixture(true, unrelated_transmission_row());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    assert!(dl.remove_owned(&torrent(), true).is_err());
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.contains("torrent-remove"))
    );
}

#[test]
fn qbit_magnet_hash_does_not_delete_same_title_different_hash() {
    let magnet = torrent_with("magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let (url, requests) = fixture(false, unrelated_qbit_row());
    let dl = QbitDownloader::connect(QbitConfig {
        url,
        username: "u".into(),
        password: "p".into(),
        category: None,
        path_maps: vec![],
    })
    .unwrap();
    dl.remove_owned(&magnet, true).unwrap();
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.contains("torrents/delete"))
    );
}

#[test]
fn transmission_magnet_hash_does_not_delete_same_title_different_hash() {
    let magnet = torrent_with("magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let (url, requests) = fixture(true, unrelated_transmission_row());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    dl.remove_owned(&magnet, true).unwrap();
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.contains("torrent-remove"))
    );
}

#[test]
fn qbit_marked_http_task_survives_restart_and_deletes_exact_hash() {
    let original = torrent();
    let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let tag = downloader::ownership_tag(&original.enclosure);
    let (url, requests) = fixture(
        false,
        json!([
            {"hash": hash, "name":"Renamed", "size":999, "progress":1, "save_path":"/downloads", "tags":tag},
            {"hash":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "name":original.title, "size":100, "progress":1, "save_path":"/downloads", "tags":"crawler-media"}
        ]),
    );
    let dl = QbitDownloader::connect(QbitConfig {
        url,
        username: "u".into(),
        password: "p".into(),
        category: None,
        path_maps: vec![],
    })
    .unwrap();
    dl.remove_owned(&original, true).unwrap();
    let requests = requests.lock().unwrap();
    let delete = requests
        .iter()
        .find(|r| r.contains("torrents/delete"))
        .unwrap();
    assert!(delete.contains(&format!("hashes={hash}&deleteFiles=true")));
}

#[test]
fn transmission_labeled_http_task_survives_restart_and_deletes_hash_not_reused_id() {
    let original = torrent();
    let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let tag = downloader::ownership_tag(&original.enclosure);
    let (url, requests) = fixture(
        true,
        json!([
            {"id":1, "hashString":hash, "name":"Renamed", "labels":[tag]},
            {"id":9, "hashString":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "name":original.title, "sizeWhenDone":100}
        ]),
    );
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    dl.remove_owned(&original, true).unwrap();
    let requests = requests.lock().unwrap();
    let delete = requests
        .iter()
        .find(|r| r.contains("torrent-remove"))
        .unwrap();
    assert!(delete.contains(&format!("\"ids\":[\"{hash}\"]")));
}

#[test]
fn strict_magnet_identity_normalizes_hex_base32_and_rejects_conflicts() {
    assert_eq!(
        downloader::magnet_info_hash("magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        Some("0000000000000000000000000000000000000000".into())
    );
    assert_eq!(
        downloader::magnet_info_hash(
            "magnet:?xt=urn%3Abtih%3AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        ),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into())
    );
    assert!(downloader::magnet_info_hash("magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&xt=urn:btih:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").is_none());
}

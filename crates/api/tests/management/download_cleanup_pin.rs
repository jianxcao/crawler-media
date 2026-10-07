//! Pin regressions: freeze the planned client/hash, and fail closed when a
//! requested reference cannot be proven distinct from a shared task.
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use api::{ApiState, DownloaderEnv, DynamicDownloader, Store, router};
use axum::http::StatusCode;
use domain::{DownloaderId, SubscribeId, Torrent};
use downloader::{Downloader, DownloaderError};
use indexer::ProfileSet;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

#[derive(Default)]
struct PartialIdentityDownloads {
    live: Mutex<HashMap<String, Result<String, String>>>,
    deleted: Mutex<Vec<String>>,
}

impl Downloader for PartialIdentityDownloads {
    fn add(&self, _: &Torrent) -> Result<(), DownloaderError> {
        Ok(())
    }

    fn completed_files(&self, _: &Torrent) -> Result<Vec<std::path::PathBuf>, DownloaderError> {
        Ok(Vec::new())
    }

    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        match self.live.lock().get(&torrent.enclosure).cloned() {
            Some(Ok(hash)) => Ok(Some(hash)),
            Some(Err(reason)) => Err(DownloaderError::Message(reason)),
            None => Ok(None),
        }
    }

    fn remove_owned(&self, torrent: &Torrent, _: bool) -> Result<(), DownloaderError> {
        self.deleted.lock().push(torrent.enclosure.clone());
        Ok(())
    }

    fn delete_task(&self, info_hash: &str, _: bool) -> Result<(), DownloaderError> {
        self.deleted.lock().push(info_hash.to_string());
        Ok(())
    }
}

fn pending(enclosure: &str) -> api::store::PendingDownload {
    api::store::PendingDownload {
        torrent: Torrent {
            site_id: domain::SiteId::new(),
            title: "The.Matrix.1999.1080p".into(),
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
        },
        release_override: None,
        downloader_id: None,
        submitted_at: Some(1),
    }
}

async fn delete_response(app: &axum::Router, id: SubscribeId) -> axum::response::Response {
    app.clone()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/subscriptions/{id}?delete_torrents=true"),
            Some("management-secret"),
            Value::Null,
        ))
        .await
        .unwrap()
}

fn subscribe_id(value: &Value) -> SubscribeId {
    value["id"].as_str().unwrap().parse().unwrap()
}

fn qbit_row(
    id: DownloaderId,
    name: &str,
    url: String,
    is_default: bool,
) -> api::store::DownloaderRow {
    api::store::DownloaderRow {
        id,
        name: name.into(),
        kind: "qbittorrent".into(),
        url,
        username: Some("u".into()),
        password: Some("p".into()),
        category: None,
        path_maps: Vec::new(),
        is_default,
        enabled: true,
    }
}

fn qbit_info(hash: &str, tags: &[&str]) -> String {
    json!([{
        "hash": hash,
        "name": "The.Matrix.1999.1080p",
        "size": 100,
        "progress": 1,
        "save_path": "/downloads",
        "tags": tags.join(","),
    }])
    .to_string()
}

fn qbit_fixture(
    info_body: String,
    on_info: impl Fn() + Send + 'static,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let deletes = Arc::new(Mutex::new(Vec::new()));
    let recorded = deletes.clone();
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
            if request.contains("torrents/info") {
                on_info();
            }
            if request.contains("torrents/delete") {
                recorded.lock().push(request.clone());
            }
            let body = if request.contains("auth/login") {
                "Ok.".into()
            } else if request.contains("torrents/info") {
                info_body.clone()
            } else {
                "Ok.".into()
            };
            let extra = if request.contains("auth/login") {
                "Set-Cookie: SID=fixture\r\n"
            } else {
                ""
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, deletes)
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

#[tokio::test]
async fn failed_requested_reference_must_not_exclude_surviving_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let downloads = Arc::new(PartialIdentityDownloads::default());
    downloads.live.lock().insert(
        "https://pt.example/failed".into(),
        Err("identity lookup failed".into()),
    );
    downloads
        .live
        .lock()
        .insert("https://pt.example/proven".into(), Ok("shared-task".into()));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloads.clone(),
    ));
    let store = Store::open(tmp.path().join("data")).unwrap();
    let subscribe = subscribe_id(&create_subscribe(&app, "search").await);
    store
        .record_pending_submission(subscribe, 80, &pending("https://pt.example/failed"))
        .unwrap();
    store
        .record_pending_submission(subscribe, 80, &pending("https://pt.example/proven"))
        .unwrap();
    let response = delete_response(&app, subscribe).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        downloads.deleted.lock().is_empty(),
        "a failed requested reference must fail closed and protect the shared task"
    );
    assert_eq!(store.load_pending(subscribe).unwrap().len(), 2);
}

#[tokio::test]
async fn unknown_requested_http_reference_must_still_protect() {
    let tmp = tempfile::tempdir().unwrap();
    let downloads = Arc::new(PartialIdentityDownloads::default());
    downloads
        .live
        .lock()
        .insert("https://pt.example/proven".into(), Ok("shared-task".into()));
    let app = router(state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: Mutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        downloads.clone(),
    ));
    let store = Store::open(tmp.path().join("data")).unwrap();
    let subscribe = subscribe_id(&create_subscribe(&app, "search").await);
    for enclosure in ["https://pt.example/unknown", "https://pt.example/proven"] {
        store
            .record_pending_submission(subscribe, 80, &pending(enclosure))
            .unwrap();
    }
    let response = delete_response(&app, subscribe).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        downloads.deleted.lock().is_empty(),
        "an unknown HTTP reference must fail closed and protect the shared task"
    );
    assert_eq!(store.load_pending(subscribe).unwrap().len(), 2);
}

struct SwitchFixture {
    store: Arc<Mutex<Store>>,
    app: axum::Router,
    deletes_a: Arc<Mutex<Vec<String>>>,
    deletes_b: Arc<Mutex<Vec<String>>>,
    hash_a: &'static str,
}

fn switch_fixture(tmp: &tempfile::TempDir) -> SwitchFixture {
    let store = Arc::new(Mutex::new(Store::open(tmp.path().join("data")).unwrap()));
    let id_a = DownloaderId::new();
    let id_b = DownloaderId::new();
    let hash_a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let info_hits = Arc::new(AtomicUsize::new(0));
    let switch_store = store.clone();
    let (url_a, deletes_a) = qbit_fixture(
        qbit_info(
            hash_a,
            &[
                downloader::TASK_TAG,
                &downloader::ownership_tag("https://pt.example/a"),
                &downloader::ownership_tag("https://pt.example/b"),
            ],
        ),
        {
            let info_hits = info_hits.clone();
            move || {
                if info_hits.fetch_add(1, Ordering::SeqCst) == 1 {
                    switch_store.lock().set_default_downloader(id_b).unwrap();
                }
            }
        },
    );
    let (url_b, deletes_b) = qbit_fixture(
        qbit_info(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &[
                downloader::TASK_TAG,
                &downloader::ownership_tag("https://pt.example/a"),
            ],
        ),
        || {},
    );
    store
        .lock()
        .insert_downloader(&qbit_row(id_a, "a", url_a, true))
        .unwrap();
    store
        .lock()
        .insert_downloader(&qbit_row(id_b, "b", url_b, false))
        .unwrap();
    let app = router(
        ApiState::new_arc(
            store.clone(),
            "management-secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(Fixtures {
                requests: Mutex::new(Vec::new()),
                bodies: HashMap::new(),
            }),
            Arc::new(DynamicDownloader::new(
                store.clone(),
                DownloaderEnv::default(),
                tmp.path(),
            )),
            tmp.path().join("library"),
        )
        .unwrap(),
    );
    SwitchFixture {
        store,
        app,
        deletes_a,
        deletes_b,
        hash_a,
    }
}

#[tokio::test]
async fn cleanup_keeps_planned_client_after_default_downloader_switch() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = switch_fixture(&tmp);
    let subscribe = subscribe_id(&create_subscribe(&fixture.app, "search").await);
    for enclosure in ["https://pt.example/a", "https://pt.example/b"] {
        fixture
            .store
            .lock()
            .record_pending_submission(subscribe, 80, &pending(enclosure))
            .unwrap();
    }
    let response = delete_response(&fixture.app, subscribe).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fixture.deletes_b.lock().len(),
        0,
        "cleanup must not delete on the downloader selected after planning"
    );
    assert_eq!(
        fixture.deletes_a.lock().len(),
        1,
        "planned client must receive the delete"
    );
    assert!(
        fixture.deletes_a.lock()[0].contains(fixture.hash_a),
        "delete must use the hash proven at plan time: {}",
        fixture.deletes_a.lock()[0]
    );
    assert!(
        fixture
            .store
            .lock()
            .load_pending(subscribe)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn switched_default_must_not_skip_surviving_explicit_old_route() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = switch_fixture(&tmp);
    let subscribe = subscribe_id(&create_subscribe(&fixture.app, "search").await);
    let mut survivor = fixture
        .store
        .lock()
        .get_subscribe(subscribe)
        .unwrap()
        .unwrap();
    survivor.id = SubscribeId::new();
    fixture.store.lock().insert_subscribe(&survivor).unwrap();
    let old_id = fixture
        .store
        .lock()
        .default_downloader()
        .unwrap()
        .unwrap()
        .id;
    for enclosure in ["https://pt.example/a", "https://pt.example/b"] {
        fixture
            .store
            .lock()
            .record_pending_submission(subscribe, 80, &pending(enclosure))
            .unwrap();
    }
    let mut shared = pending("https://pt.example/a");
    shared.downloader_id = Some(old_id);
    fixture
        .store
        .lock()
        .record_pending_submission(survivor.id, 80, &shared)
        .unwrap();
    let response = delete_response(&fixture.app, subscribe).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        fixture.deletes_a.lock().is_empty(),
        "task on frozen old route still has a surviving reference"
    );
    assert!(fixture.deletes_b.lock().is_empty());
    assert_eq!(
        fixture
            .store
            .lock()
            .load_pending(survivor.id)
            .unwrap()
            .len(),
        1
    );
}

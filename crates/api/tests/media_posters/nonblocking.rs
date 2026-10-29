use super::*;
use std::time::Duration;

struct StalledPoster {
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl ImageFetcher for StalledPoster {
    fn fetch(&self, _: &str) -> Result<(String, Vec<u8>), PosterError> {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        Ok(("image/png".into(), vec![0x89, 0x50, 0x4e, 0x47]))
    }
}

#[tokio::test]
async fn cold_public_media_poster_frees_http_connection_before_upstream_finishes() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let state = ApiState::new(
        Store::open(tmp.path().join("data")).unwrap(),
        "test-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NullFetcher),
        Arc::new(NullDownloader),
        tmp.path().join("library"),
    )
    .unwrap();
    let media_id = MediaId::new();
    state
        .store()
        .lock()
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: "Pending poster".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: Some("valid".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let (release, stalled) = std::sync::mpsc::channel();
    let (entered, started) = tokio::sync::oneshot::channel();
    let service = Arc::new(
        MediaPosterService::with_custom_fetcher(
            state.store(),
            Arc::new(MockCatalog {
                poster_calls: AtomicUsize::new(0),
            }),
            tmp.path().join("cache"),
            Arc::new(StalledPoster {
                release: std::sync::Mutex::new(stalled),
                entered: std::sync::Mutex::new(Some(entered)),
            }),
        )
        .unwrap(),
    );
    let app = api::router(state.with_media_posters(service.clone()));
    let response = tokio::time::timeout(
        Duration::from_millis(400),
        app.oneshot(
            Request::get(format!("/api/v1/media/{media_id}/poster"))
                .body(axum::body::Body::empty())
                .unwrap(),
        ),
    )
    .await;
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), started)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while service.resource_counts().active_workers != 0 || service.inflight_entries() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let response = response
        .expect("cold posters must return before remote reads finish")
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["retry-after"], "1");
    assert_eq!(response.headers()["cache-control"], "no-store");
}

#[tokio::test]
async fn cached_public_media_poster_returns_etag_and_not_modified() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let state = ApiState::new(
        Store::open(tmp.path().join("data")).unwrap(),
        "test-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NullFetcher),
        Arc::new(NullDownloader),
        tmp.path().join("library"),
    )
    .unwrap();
    let cache_dir = tmp.path().join("cache");
    let id = MediaId::new();
    api::media_posters::cache::DiskPosterCache::new(&cache_dir)
        .put(id, "image/png", &[0x89, 0x50, 0x4e, 0x47])
        .unwrap();
    let service = Arc::new(
        MediaPosterService::with_custom_fetcher(
            state.store(),
            Arc::new(MockCatalog {
                poster_calls: AtomicUsize::new(0),
            }),
            cache_dir,
            Arc::new(MockFetcher {
                fetch_calls: AtomicUsize::new(0),
            }),
        )
        .unwrap(),
    );
    let app = api::router(state.with_media_posters(service.clone()));
    let response = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/media/{id}/poster"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response
        .headers()
        .get("etag")
        .expect("cached posters need validators");
    let response = app
        .oneshot(
            Request::get(format!("/api/v1/media/{id}/poster"))
                .header("if-none-match", etag)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert!(
        axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn media_poster_warmup_reads_identity_without_the_writer_lock() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let state = ApiState::new(
        Store::open(tmp.path().join("data")).unwrap(),
        "test-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NullFetcher),
        Arc::new(NullDownloader),
        tmp.path().join("library"),
    )
    .unwrap();
    let media_id = MediaId::new();
    state
        .store()
        .lock()
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: "Writer busy".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: Some("valid".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let service = Arc::new(
        MediaPosterService::with_custom_fetcher(
            state.store(),
            Arc::new(MockCatalog {
                poster_calls: AtomicUsize::new(0),
            }),
            tmp.path().join("cache"),
            Arc::new(MockFetcher {
                fetch_calls: AtomicUsize::new(0),
            }),
        )
        .unwrap(),
    );
    let handle = state.store();
    let app = api::router(state.with_media_posters(service.clone()));
    let (locked, lock_ready) = std::sync::mpsc::channel();
    let (release, finish) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = handle.lock();
        locked.send(()).unwrap();
        finish.recv_timeout(Duration::from_secs(3)).unwrap();
    });
    lock_ready.recv_timeout(Duration::from_secs(1)).unwrap();
    let response = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/api/v1/media/{media_id}/poster"))
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            if response.status() == StatusCode::OK {
                break response;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    release.send(()).unwrap();
    holder.join().unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while service.resource_counts().active_workers != 0 || service.inflight_entries() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        response
            .expect("poster warmup must not wait for the writer")
            .status(),
        StatusCode::OK
    );
}

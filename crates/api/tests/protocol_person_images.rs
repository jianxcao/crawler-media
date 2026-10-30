//! Person artwork returns complete cached bytes through the shared image service.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use api::{ApiState, Store};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Media, MediaId, MediaKind};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use tower::ServiceExt;

const TOKEN: &str = "person-images";
const IMAGE: &[u8] = b"\x89PNG\r\n\x1a\nfinite person image bytes";
type ImageFetch = Arc<dyn Fn(&str) -> Result<(String, Vec<u8>), String> + Send + Sync>;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    state: ApiState,
    library: String,
}
impl Fixture {
    fn new(fetch: ImageFetch) -> Self {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let library = store
            .create_library(
                MediaKind::Movie,
                "People",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
            .id;
        publish_actor(&store, &root, "Jane Roe", "jane");
        let hidden_root = dir.path().join("hidden");
        std::fs::create_dir_all(&hidden_root).unwrap();
        store
            .create_library(
                MediaKind::Movie,
                "Hidden",
                &[hidden_root.to_str().unwrap()],
                "members",
                false,
                &[],
            )
            .unwrap();
        publish_actor(&store, &hidden_root, "Hidden Person", "hidden");
        let state = ApiState::new(
            store,
            TOKEN.into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap()
        .with_image_fetch(fetch);
        Self {
            _dir: dir,
            state,
            library,
        }
    }
    fn app(&self) -> axum::Router {
        api::router(self.state.clone())
    }
}
fn publish_actor(store: &Store, root: &std::path::Path, actor: &str, image: &str) {
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: actor.into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: vec![],
    };
    store.insert_media(&media).unwrap();
    let path = root.join(format!("{image}.mkv"));
    std::fs::write(&path, b"local movie").unwrap();
    std::fs::write(path.with_extension("nfo"), format!("<movie><title>{actor}</title><actor><name>{actor}</name><role>Lead</role><thumb>https://image.tmdb.org/t/p/w185/{image}.png</thumb></actor></movie>")).unwrap();
    store::test_support::publish_fixture_file(
        store,
        &store::test_support::FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
}
fn request(uri: &str, authenticated: bool, etag: Option<&str>) -> Request<Body> {
    let mut request = Request::builder().uri(uri);
    if authenticated {
        request = request.header("Authorization", format!("Bearer {TOKEN}"));
    }
    if let Some(etag) = etag {
        request = request.header("If-None-Match", etag);
    }
    request.body(Body::empty()).unwrap()
}
async fn image(app: axum::Router, uri: impl Into<String>) -> axum::response::Response {
    let uri = uri.into();
    app.oneshot(request(&uri, true, None)).await.unwrap()
}
async fn assert_image(response: axum::response::Response) -> String {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["Content-Type"], "image/png");
    assert!(
        response.headers().get("Location").is_none(),
        "complete image delivery must not redirect to origin"
    );
    let etag = response.headers()["ETag"].to_str().unwrap().to_string();
    assert_eq!(
        &to_bytes(response.into_body(), 1024).await.unwrap()[..],
        IMAGE
    );
    etag
}
fn success() -> ImageFetch {
    Arc::new(|_| Ok(("image/png".into(), IMAGE.to_vec())))
}
fn person_id() -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"name:jane roe:Actor")
        .hyphenated()
        .to_string()
}

#[tokio::test]
async fn person_image_warm_read_serves_cached_bytes() {
    let fixture = Fixture::new(success());
    let uri = "/Persons/Jane%20Roe/Images/Primary";
    assert_image(image(fixture.app(), uri).await).await;
    // A fresh delivery service must reuse durable bytes, even when its origin fails.
    let offline = fixture
        .state
        .clone()
        .with_image_fetch(Arc::new(|_| Err("origin is now unavailable".into())));
    let app = api::router(offline);
    let etag = assert_image(image(app.clone(), uri).await).await;
    assert_image(
        image(
            app.clone(),
            format!("/Items/{}/Images/Primary", person_id()),
        )
        .await,
    )
    .await;
    // Public image proxy and Person delivery share the same durable URL cache.
    assert_image(
        image(
            app.clone(),
            "/api/v1/images/proxy?url=https%3A%2F%2Fimage.tmdb.org%2Ft%2Fp%2Fw185%2Fjane.png",
        )
        .await,
    )
    .await;
    let conditional = app.oneshot(request(uri, true, Some(&etag))).await.unwrap();
    assert_eq!(conditional.status(), StatusCode::NOT_MODIFIED);
    assert!(
        to_bytes(conditional.into_body(), 1024)
            .await
            .unwrap()
            .is_empty()
    );
}

struct Gate {
    released: Mutex<bool>,
    changed: Condvar,
    entered: AtomicBool,
    spent: AtomicBool,
    only_one_payload: bool,
}
impl Gate {
    fn new(only_one_payload: bool) -> Arc<Self> {
        Arc::new(Self {
            released: Mutex::new(false),
            changed: Condvar::new(),
            entered: AtomicBool::new(false),
            spent: AtomicBool::new(false),
            only_one_payload,
        })
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
    fn fetch(&self) -> Result<(String, Vec<u8>), String> {
        self.entered.store(true, Ordering::SeqCst);
        let (released, timeout) = self
            .changed
            .wait_timeout_while(
                self.released.lock().unwrap(),
                Duration::from_secs(5),
                |open| !*open,
            )
            .unwrap();
        if timeout.timed_out() && !*released {
            return Err("finite origin deadline".into());
        }
        drop(released);
        if self.only_one_payload && self.spent.swap(true, Ordering::SeqCst) {
            return Err("finite origin has no second payload".into());
        }
        Ok(("image/png".into(), IMAGE.to_vec()))
    }
    fn transport(self: &Arc<Self>) -> ImageFetch {
        let source = self.clone();
        Arc::new(move |_| source.fetch())
    }
}
struct Release(Arc<Gate>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
async fn entered(gate: &Gate) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while !gate.entered.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "finite image origin was not reached"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn person_image_fill_is_shared_and_never_returns_warming_503() {
    let source = Gate::new(true);
    let _release = Release(source.clone());
    let fixture = Fixture::new(source.transport());
    let a = tokio::spawn(image(fixture.app(), "/Persons/Jane%20Roe/Images/Primary"));
    entered(&source).await;
    let b = tokio::spawn(image(
        fixture.app(),
        format!("/Items/{}/Images/Primary", person_id()),
    ));
    tokio::task::yield_now().await;
    source.release();
    assert_image(a.await.unwrap()).await;
    assert_image(b.await.unwrap()).await;
}

#[tokio::test]
async fn person_image_resolution_respects_library_visibility() {
    let fixture = Fixture::new(success());
    let uri = "/Persons/Jane%20Roe/Images/Primary";
    let etag = assert_image(image(fixture.app(), uri).await).await;
    let public = fixture
        .app()
        .oneshot(request(uri, false, None))
        .await
        .unwrap();
    assert_image(public).await;
    assert_eq!(
        image(fixture.app(), "/Persons/Hidden%20Person/Images/Primary")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    fixture
        .state
        .store()
        .begin_library_removal(&fixture.library)
        .unwrap();
    for authenticated in [true, false] {
        let response = fixture
            .app()
            .oneshot(request(uri, authenticated, Some(&etag)))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "visibility must be checked before a warm validator hit"
        );
    }
}

#[tokio::test]
async fn stalled_person_image_source_has_bounded_completion() {
    let source = Gate::new(false);
    let _release = Release(source.clone());
    let fixture = Fixture::new(source.transport());
    let start = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        image(fixture.app(), "/Persons/Jane%20Roe/Images/Primary"),
    )
    .await;
    source.release();
    let response = result.expect("an observer must finish even when the finite origin is stalled");
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(start.elapsed() < Duration::from_secs(3));
    assert!(
        !to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .starts_with(b"\x89PNG")
    );
    // The worker may still publish after this observer's deadline. Observe
    // recovery through HTTP; joining that expired flight may legitimately return
    // 504 until its actual completion. This is an eventual recovery assertion,
    // not a retry added to the production delivery path.
    let recovered = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let response = image(fixture.app(), "/Persons/Jane%20Roe/Images/Primary").await;
            if response.status() == StatusCode::OK {
                break response;
            }
            assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
            assert!(
                !to_bytes(response.into_body(), 1024)
                    .await
                    .unwrap()
                    .starts_with(b"\x89PNG")
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("completed origin work must restore usable cached delivery");
    assert_image(recovered).await;
}

#[tokio::test]
async fn cancelled_person_image_observer_does_not_discard_shared_fill() {
    let source = Gate::new(true);
    let _release = Release(source.clone());
    let fixture = Fixture::new(source.transport());
    let pending = tokio::spawn(image(fixture.app(), "/Persons/Jane%20Roe/Images/Primary"));
    entered(&source).await;
    pending.abort();
    let replacement = tokio::spawn(image(
        fixture.app(),
        format!("/Items/{}/Images/Primary", person_id()),
    ));
    tokio::task::yield_now().await;
    source.release();
    assert_image(replacement.await.unwrap()).await;
}

#[tokio::test]
async fn person_image_hidden_during_fill_is_not_published() {
    for etag in [None, Some("*")] {
        hidden_during_fill(etag).await;
    }
}

async fn hidden_during_fill(etag: Option<&str>) {
    let source = Gate::new(false);
    let _release = Release(source.clone());
    let fixture = Fixture::new(source.transport());
    let app = fixture.app();
    let pending_request = request("/Persons/Jane%20Roe/Images/Primary", true, etag);
    let observer = app.clone();
    let pending = tokio::spawn(async move { observer.oneshot(pending_request).await.unwrap() });
    entered(&source).await;
    fixture
        .state
        .store()
        .begin_library_removal(&fixture.library)
        .unwrap();
    source.release();
    let response = pending.await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(
        !to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .starts_with(b"\x89PNG")
    );
    // The completed shared fill may be cached, but visibility still precedes
    // validator handling on both Person name and generic Item identity routes.
    for uri in [
        "/Persons/Jane%20Roe/Images/Primary".to_string(),
        format!("/Items/{}/Images/Primary", person_id()),
    ] {
        let response = app
            .clone()
            .oneshot(request(&uri, true, Some("*")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            !to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .starts_with(b"\x89PNG")
        );
    }
}

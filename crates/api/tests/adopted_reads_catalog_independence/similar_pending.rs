use super::*;

struct GatedSimilar {
    gate: Arc<Gate>,
    result: Result<Vec<CatalogHit>, String>,
    cached: Mutex<Option<Vec<CatalogHit>>>,
}
impl Catalog for GatedSimilar {
    fn search_movie(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn cached_similar(&self, _: MediaKind, _: &str) -> Result<Option<Vec<CatalogHit>>, String> {
        Ok(self.cached.lock().unwrap().clone())
    }
    fn similar(&self, _: MediaKind, _: &str) -> Result<Vec<CatalogHit>, String> {
        self.gate.stall();
        if let Ok(hits) = &self.result {
            *self.cached.lock().unwrap() = Some(hits.clone());
        }
        self.result.clone()
    }
}
fn hit() -> CatalogHit {
    CatalogHit {
        media: Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Complete recommendation".into(),
            year: Some(2026),
            original_title: None,
            aliases: vec![],
            tmdb_id: Some("42".into()),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        },
        poster_path: Some("/fixture.jpg".into()),
        backdrop_path: None,
        rating: None,
        overview: None,
    }
}
fn url(fx: &Fixture) -> String {
    format!(
        "/api/v1/libraries/{}/items/{}/similar",
        fx.library, fx.media
    )
}
async fn completed(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let response = request(app.clone(), "GET", uri).await;
            if response.0 != StatusCode::ACCEPTED {
                return response;
            }
            assert_eq!(response.1["data"]["state"], "pending");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("finite catalog must eventually publish ready or error")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn similar_cold_request_is_explicitly_pending_then_returns_complete_result() {
    let fx = fixture();
    let gate = Arc::new(Gate::default());
    let state = fx.state.clone().with_catalog(Arc::new(GatedSimilar {
        gate: gate.clone(),
        cached: Mutex::new(None),
        result: Ok(vec![hit()]),
    }));
    let app = api::router(state);
    let first = tokio::time::timeout(
        Duration::from_millis(150),
        request(app.clone(), "GET", &url(&fx)),
    )
    .await;
    gate.release();
    let first =
        first.expect("uncached recommendations must not block the detail response on origin");
    assert_eq!(first.0, StatusCode::ACCEPTED);
    assert_eq!(first.1["data"]["state"], "pending");
    let retry_ms = first.1["data"]["retry_after_ms"].as_u64().unwrap();
    assert!(
        (1..=100).contains(&retry_ms),
        "pending retry hint must be at most 100ms"
    );
    assert!(
        first.1["data"].get("items").is_none(),
        "pending must not pretend to be an empty ready result"
    );
    let response = completed(app, &url(&fx)).await;
    assert_eq!(response.0, StatusCode::OK);
    assert_eq!(response.1["data"]["state"], "ready");
    assert_eq!(
        response.1["data"]["items"][0]["title"],
        "Complete recommendation"
    );
    assert_eq!(
        response.1["data"]["items"][0]["poster_url"],
        "https://image.tmdb.org/t/p/w342/fixture.jpg"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn similar_pending_failure_is_published_as_error_never_successful_empty_data() {
    let fx = fixture();
    let gate = Arc::new(Gate::default());
    let app = api::router(fx.state.clone().with_catalog(Arc::new(GatedSimilar {
        gate: gate.clone(),
        cached: Mutex::new(None),
        result: Err("finite upstream timeout".into()),
    })));
    let first = tokio::time::timeout(
        Duration::from_millis(150),
        request(app.clone(), "GET", &url(&fx)),
    )
    .await;
    gate.release();
    assert_eq!(
        first
            .expect("a source failure starts as explicit pending")
            .0,
        StatusCode::ACCEPTED
    );
    let response = completed(app, &url(&fx)).await;
    assert_eq!(response.0, StatusCode::BAD_GATEWAY);
    assert_eq!(response.1["ok"], false);
    assert!(response.1.get("data").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn similar_pending_completion_does_not_bypass_library_removal_visibility() {
    let fx = fixture();
    let gate = Arc::new(Gate::default());
    let app = api::router(fx.state.clone().with_catalog(Arc::new(GatedSimilar {
        gate: gate.clone(),
        cached: Mutex::new(None),
        result: Ok(vec![hit()]),
    })));
    let first = tokio::time::timeout(
        Duration::from_millis(150),
        request(app.clone(), "GET", &url(&fx)),
    )
    .await;
    gate.release();
    assert_eq!(
        first.expect("cold request must return pending").0,
        StatusCode::ACCEPTED
    );
    fx.state.store().begin_library_removal(&fx.library).unwrap();
    let response = request(app, "GET", &url(&fx)).await;
    assert_eq!(response.0, StatusCode::NOT_FOUND);
    assert!(response.1.get("data").is_none());
}

struct ResourceCatalog {
    gate: Arc<Gate>,
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
    cached: Mutex<std::collections::HashMap<String, Vec<CatalogHit>>>,
}
impl Catalog for ResourceCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        Ok(None)
    }
    fn cached_similar(&self, _: MediaKind, id: &str) -> Result<Option<Vec<CatalogHit>>, String> {
        Ok(self.cached.lock().unwrap().get(id).cloned())
    }
    fn similar(&self, _: MediaKind, id: &str) -> Result<Vec<CatalogHit>, String> {
        use std::sync::atomic::Ordering;
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        self.gate.stall();
        let hits = vec![hit()];
        self.cached.lock().unwrap().insert(id.into(), hits.clone());
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(hits)
    }
}
fn distinct_target(fx: &Fixture, index: usize) -> String {
    let store = fx.state.store();
    let mut media = hit().media;
    media.id = MediaId::new();
    media.tmdb_id = Some(format!("resource-{index}"));
    store.insert_media(&media).unwrap();
    let path = fx
        ._dir
        .path()
        .join(format!("tv/Adopted/resource-{index}.mkv"));
    std::fs::write(&path, b"fixture").unwrap();
    store::test_support::publish_fixture_file(
        &store,
        &store::test_support::FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    format!(
        "/api/v1/libraries/{}/items/{}/similar",
        fx.library, media.id
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn similar_refresh_resources_are_bounded_and_capacity_recovers() {
    use std::sync::atomic::Ordering;
    let fx = fixture();
    let uris = (0..65)
        .map(|index| distinct_target(&fx, index))
        .collect::<Vec<_>>();
    let gate = Arc::new(Gate::default());
    let catalog = Arc::new(ResourceCatalog {
        gate: gate.clone(),
        active: 0.into(),
        peak: 0.into(),
        cached: Mutex::default(),
    });
    let app = api::router(fx.state.clone().with_catalog(catalog.clone()));
    for uri in &uris[..64] {
        let response = request(app.clone(), "GET", uri).await;
        assert_eq!(response.0, StatusCode::ACCEPTED);
        assert_eq!(response.1["data"]["state"], "pending");
    }
    for _ in 0..5 {
        assert_eq!(
            request(app.clone(), "GET", &uris[0]).await.0,
            StatusCode::ACCEPTED,
            "joining the same key must not consume capacity"
        );
    }
    assert_eq!(
        request(app.clone(), "GET", &uris[64]).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(catalog.peak.load(Ordering::SeqCst) > 0);
    assert!(
        catalog.peak.load(Ordering::SeqCst) <= 4,
        "actual synchronous source work is bounded independently of callers"
    );
    gate.release();
    for uri in &uris[..64] {
        let response = completed(app.clone(), uri).await;
        assert_eq!(response.0, StatusCode::OK);
        assert_eq!(response.1["data"]["items"].as_array().unwrap().len(), 1);
    }
    let recovered = completed(app, &uris[64]).await;
    assert_eq!(recovered.0, StatusCode::OK);
    assert_eq!(
        recovered.1["data"]["items"][0]["title"],
        "Complete recommendation"
    );
    assert_eq!(catalog.active.load(Ordering::SeqCst), 0);
}

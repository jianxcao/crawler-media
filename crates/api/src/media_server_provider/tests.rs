use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::provider::QueryTiming;
use tower::ServiceExt;

use crate::{ApiState, Store, router};

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

#[test]
fn readonly_page_keeps_original_token_and_skips_incidental_device_binding() {
    let dir = tempfile::tempdir().unwrap();
    let state = ApiState::new(
        Store::open(dir.path().join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
        dir.path().join("library"),
    )
    .unwrap();
    let user_id = state
        .store
        .user_id_by_token("management-secret")
        .unwrap()
        .unwrap();
    state.store.set_user_token(user_id, "device-token").unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let app = router(state.clone());
        let request = || {
            let started = Instant::now();
            let mut request = Request::get("/Items")
                .header("authorization", "Bearer device-token")
                .header("X-Emby-Device-Id", "bedroom")
                .body(Body::empty())
                .unwrap();
            request.extensions_mut().insert(QueryTiming {
                started,
                deadline: started + Duration::from_secs(1),
            });
            request
        };
        state.instance_writable.store(false, Ordering::SeqCst);
        let response = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!state.store.device_is_revocable(user_id, "bedroom").unwrap());
        state.instance_writable.store(true, Ordering::SeqCst);
        for _ in 0..2 {
            let response = app.clone().oneshot(request()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert!(state.store.device_is_revocable(user_id, "bedroom").unwrap());
    });
}

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::{Extension, Path, State},
    http::{Request, StatusCode},
    middleware,
    routing::get,
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use tower::ServiceExt;

use crate::{ApiState, Store};
use store::{
    LibraryReadError, LibraryReadRequest, ReadCredential,
    library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery},
};

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

async fn credential_handler(
    State(state): State<ApiState>,
    Extension(credential): Extension<ReadCredential>,
    Extension(user_id): Extension<domain::UserId>,
    Path(id): Path<String>,
) -> StatusCode {
    let store = state.store.lock();
    if id == "revoke-bearer" {
        store
            .delete_session_tokens(&["bearer-token".into()])
            .unwrap();
    }
    let request = LibraryReadRequest::Browse(LibraryQuery {
        library_id: domain::LibraryId::new(),
        user_id: Some(user_id),
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset { start: 0, limit: 1 },
        include_total: true,
    });
    match store.prepare_library_read(&credential, &request) {
        Ok(_) => StatusCode::OK,
        Err(LibraryReadError::Unauthorized) => StatusCode::UNAUTHORIZED,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[test]
fn self_auth_passes_selected_bearer_or_cookie_as_opaque_read_credential() {
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
    let user = state
        .store
        .lock()
        .user_id_by_token("management-secret")
        .unwrap()
        .unwrap();
    for token in ["bearer-token", "cookie-token"] {
        state.store.lock().set_user_token(user, token).unwrap();
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let app = Router::new()
            .route("/libraries/{id}/items", get(credential_handler))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                super::authenticate,
            ))
            .route_layer(middleware::from_fn(super::super::query_timing))
            .with_state(state.clone());
        let response = app
            .clone()
            .oneshot(
                Request::get("/libraries/revoke-bearer/items")
                    .header("authorization", "Bearer bearer-token")
                    .header("cookie", "mc_session=cookie-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            state.store.lock().user_id_by_token("cookie-token").unwrap(),
            Some(user)
        );
        let response = app
            .oneshot(
                Request::get("/libraries/cookie-only/items")
                    .header("cookie", "mc_session=cookie-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    });
}

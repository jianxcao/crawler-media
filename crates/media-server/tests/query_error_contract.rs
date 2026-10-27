#[path = "support/mod.rs"]
mod support;
use support::fixture_provider;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::UserId;
use media_server::provider::{
    AuthenticateError, MediaItemSnapshot, MediaServerProvider, PlaybackClientInfo, PlaybackEvent,
    PlaybackEventIdentity, ServerLibrary, ServerUser,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tower::ServiceExt;

struct FileAssetProvider {
    user_id: UserId,
    dummy_video_path: PathBuf,
    observed: Mutex<Vec<(&'static str, String, Option<Instant>)>>,
}

impl FileAssetProvider {
    fn select_segment(
        &self,
        principal: media_server::provider::QueryPrincipal,
        query: media_server::provider::ServerItemQuery,
    ) -> Result<media_server::provider::ServerItemPage, media_server::provider::QueryError> {
        assert_eq!(principal.user_id, self.user_id);
        assert!(!format!("{principal:?}").contains("valid-token"));
        self.observed
            .lock()
            .unwrap()
            .push(("query", principal.token, Some(principal.deadline)));
        if query.parent_id.as_deref() == Some("latest-two") {
            return Ok(media_server::provider::ServerItemPage::default());
        }
        let parent = query.parent_id.as_deref().unwrap_or_default();
        if parent.starts_with("latest-second-") && query.is_played == Some(false) {
            return Ok(media_server::provider::ServerItemPage::default());
        }
        let parent = parent.strip_prefix("latest-second-").unwrap_or(parent);
        Err(match parent {
            "unauthorized" => media_server::provider::QueryError::Unauthorized,
            "internal" => media_server::provider::QueryError::Internal("fixture failure".into()),
            _ => media_server::provider::QueryError::Unavailable,
        })
    }
}

#[async_trait::async_trait]
impl MediaServerProvider for FileAssetProvider {

    async fn next_up_items(
        &self,
        user_id: domain::UserId,
        series_id: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<media_server::provider::ServerItemPage, String> {
        fixture_provider::next_up_items(self, user_id, series_id, offset, limit).await
    }
    async fn query_visible_items(
        &self,
        principal: media_server::provider::QueryPrincipal,
        query: media_server::provider::ServerItemQuery,
    ) -> Result<media_server::provider::ServerItemPage, media_server::provider::QueryError> {
        self.select_segment(principal, query)
    }

    async fn query_latest_items(
        &self,
        principal: media_server::provider::QueryPrincipal,
        query: media_server::provider::ServerItemQuery,
    ) -> Result<Vec<MediaItemSnapshot>, media_server::provider::QueryError> {
        let mut segment = query.clone();
        segment.is_played = Some(query.is_played.unwrap_or(false));
        let mut page = self.select_segment(principal.clone(), segment.clone())?;
        if query.is_played.is_none() && page.items.len() < query.limit {
            segment.is_played = Some(true);
            segment.offset = (query.offset + page.items.len()).saturating_sub(page.total);
            segment.limit = query.limit - page.items.len();
            page.items
                .extend(self.select_segment(principal, segment)?.items);
        }
        Ok(page.items)
    }

    fn server_id(&self) -> String {
        "test-file-assets".into()
    }
    async fn authenticate_password(
        &self,
        _username: &str,
        _password: &str,
    ) -> Result<Option<(String, ServerUser)>, AuthenticateError> {
        Ok(None)
    }
    async fn resolve_cover_bytes(
        &self,
        _user_id: Option<UserId>,
        _id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }
    async fn resolve_poster_bytes(
        &self,
        _user_id: Option<UserId>,
        _id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }
    async fn user_id_by_token(
        &self,
        token: &str,
        deadline: Option<std::time::Instant>,
    ) -> Result<Option<UserId>, media_server::provider::QueryError> {
        self.observed
            .lock()
            .unwrap()
            .push(("auth", token.into(), deadline));
        match token {
            "auth-unavailable" => Err(media_server::provider::QueryError::Unavailable),
            "auth-internal" => Err(media_server::provider::QueryError::Internal(
                "fixture".into(),
            )),
            _ => Ok((token == "valid-token").then_some(self.user_id)),
        }
    }
    async fn bind_playback_credential(
        &self,
        _user_id: UserId,
        device_id: &str,
        token: &str,
        deadline: Option<Instant>,
    ) -> Result<(), media_server::provider::QueryError> {
        self.observed
            .lock()
            .unwrap()
            .push(("bind", token.into(), deadline));
        match device_id {
            "unavailable" => Err(media_server::provider::QueryError::Unavailable),
            "internal" => Err(media_server::provider::QueryError::Internal(
                "fixture".into(),
            )),
            "unauthorized" => Err(media_server::provider::QueryError::Unauthorized),
            _ => Ok(()),
        }
    }
    async fn get_user_for_query(
        &self,
        principal: media_server::provider::QueryPrincipal,
    ) -> Result<Option<ServerUser>, media_server::provider::QueryError> {
        self.observed.lock().unwrap().push((
            "user",
            principal.token.clone(),
            Some(principal.deadline),
        ));
        self.get_user(principal.user_id)
            .await
            .map_err(media_server::provider::QueryError::Internal)
    }
    async fn list_visible_libraries_for_query(
        &self,
        principal: media_server::provider::QueryPrincipal,
    ) -> Result<Vec<ServerLibrary>, media_server::provider::QueryError> {
        self.observed.lock().unwrap().push((
            "libraries",
            principal.token.clone(),
            Some(principal.deadline),
        ));
        self.list_visible_libraries(principal.user_id)
            .await
            .map_err(media_server::provider::QueryError::Internal)
    }
    async fn get_user(&self, user_id: UserId) -> Result<Option<ServerUser>, String> {
        Ok((user_id == self.user_id).then(|| ServerUser {
            id: self.user_id,
            login: "viewer".into(),
            is_admin: false,
        }))
    }
    async fn list_visible_libraries(&self, _user_id: UserId) -> Result<Vec<ServerLibrary>, String> {
        Ok(Vec::new())
    }
    async fn list_visible_items(
        &self,
        _user_id: UserId,
        _parent_id: Option<&str>,
    ) -> Result<Vec<MediaItemSnapshot>, String> {
        Ok(Vec::new())
    }
    async fn resolve_single_item(
        &self,
        _user_id: UserId,
        _id: &str,
    ) -> Result<Option<MediaItemSnapshot>, String> {
        Ok(None)
    }
    async fn resolve_stream_source(
        &self,
        _user_id: UserId,
        id: &str,
    ) -> Result<Option<(PathBuf, bool)>, String> {
        if id == "present-file" {
            Ok(Some((self.dummy_video_path.clone(), false)))
        } else {
            Ok(None)
        }
    }
    async fn set_user_marks(
        &self,
        _user_id: UserId,
        _id: &str,
        _played: Option<bool>,
        _favorite: Option<bool>,
    ) -> Result<bool, String> {
        Ok(true)
    }
    async fn report_playback_event(
        &self,
        _user_id: UserId,
        _id: &str,
        _position_ms: i64,
        _paused: bool,
        _client: PlaybackClientInfo,
        _event: PlaybackEvent,
        _identity: PlaybackEventIdentity,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn issue_play_session(
        &self,
        _user_id: UserId,
        _device_id: &str,
        _item_id: &str,
    ) -> Result<String, String> {
        Ok(uuid::Uuid::new_v4().to_string())
    }
}

#[tokio::test]
async fn typed_query_errors_keep_http_status_on_catalog_and_episode_routes() {
    let provider = Arc::new(FileAssetProvider {
        user_id: UserId::new(),
        dummy_video_path: PathBuf::new(),
        observed: Mutex::new(Vec::new()),
    });
    let app = media_server::routes(provider);
    for (uri, status) in [
        ("/Items", StatusCode::SERVICE_UNAVAILABLE),
        ("/Items?ParentId=unauthorized", StatusCode::UNAUTHORIZED),
        (
            "/Items?ParentId=internal",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            "/Shows/w1_00000000000000000000000000000001/Episodes",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(uri)
                    .header("authorization", "Bearer valid-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{uri}");
    }
}

#[tokio::test]
async fn covered_routes_keep_typed_auth_binding_and_query_statuses() {
    let provider = Arc::new(FileAssetProvider {
        user_id: UserId::new(),
        dummy_video_path: PathBuf::new(),
        observed: Mutex::new(Vec::new()),
    });
    let app = media_server::routes(provider);
    for (uri, token, device, status) in [
        ("/Items", "invalid", "device-1", StatusCode::UNAUTHORIZED),
        (
            "/Items",
            "auth-unavailable",
            "device-1",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            "/Items",
            "auth-internal",
            "device-1",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            "/Items",
            "valid-token",
            "unavailable",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            "/Items",
            "valid-token",
            "internal",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            "/Items",
            "valid-token",
            "unauthorized",
            StatusCode::UNAUTHORIZED,
        ),
        (
            "/emby/Items?ParentId=unauthorized",
            "valid-token",
            "device-1",
            StatusCode::UNAUTHORIZED,
        ),
        (
            "/emby/Items?ParentId=internal",
            "valid-token",
            "device-1",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            "/Shows/unauthorized/Episodes",
            "valid-token",
            "device-1",
            StatusCode::UNAUTHORIZED,
        ),
        (
            "/emby/Shows/internal/Episodes",
            "valid-token",
            "device-1",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(uri)
                    .header("authorization", format!("Bearer {token}"))
                    .header("X-Emby-Device-Id", device)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{uri}");
    }
}

#[tokio::test]
async fn latest_second_segment_and_missing_timing_reuse_one_principal() {
    let provider = Arc::new(FileAssetProvider {
        user_id: UserId::new(),
        dummy_video_path: PathBuf::new(),
        observed: Mutex::new(Vec::new()),
    });
    let app = media_server::routes(provider.clone());
    let response = app
        .clone()
        .oneshot(
            Request::get("/Users/viewer/Items/Latest?ParentId=latest-two")
                .header("authorization", "Bearer valid-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let calls = provider.observed.lock().unwrap();
    let query: Vec<_> = calls
        .iter()
        .filter(|(phase, _, _)| *phase == "query")
        .collect();
    assert_eq!(query.len(), 2);
    let auth = calls.iter().find(|(phase, _, _)| *phase == "auth").unwrap();
    assert_eq!(query[0].2, auth.2);
    assert_eq!(query[1].2, auth.2);
    assert!(
        calls
            .iter()
            .all(|(_, token, deadline)| token == "valid-token" && deadline.is_some())
    );
}

#[tokio::test]
async fn covered_head_routes_infer_timing_and_keep_status() {
    let provider = Arc::new(FileAssetProvider {
        user_id: UserId::new(),
        dummy_video_path: PathBuf::new(),
        observed: Mutex::new(Vec::new()),
    });
    let app = media_server::routes(provider.clone());
    for uri in [
        "/Items",
        "/emby/Items",
        "/Shows/w1_00000000000000000000000000000001/Episodes",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri(uri)
                    .header("authorization", "Bearer valid-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
    }
    assert!(
        provider
            .observed
            .lock()
            .unwrap()
            .iter()
            .all(|(_, _, deadline)| deadline.is_some())
    );
}

#[tokio::test]
async fn query_unavailable_is_503_and_principal_keeps_original_credential() {
    let provider = Arc::new(FileAssetProvider {
        user_id: UserId::new(),
        dummy_video_path: PathBuf::new(),
        observed: Mutex::new(Vec::new()),
    });
    let app = media_server::routes(provider.clone());
    let started = Instant::now();
    let deadline = started + Duration::from_millis(250);
    for uri in [
        "/Items",
        "/emby/Items",
        "/Users/me/Items/Latest",
        "/Shows/w1_00000000000000000000000000000001/Episodes",
        "/emby/Shows/w1_00000000000000000000000000000001/Episodes",
    ] {
        let mut request = Request::get(uri)
            .header("authorization", "Bearer valid-token")
            .header("X-Emby-Device-Id", "device-1")
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(media_server::provider::QueryTiming { started, deadline });
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
    }
    let calls = provider.observed.lock().unwrap();
    assert!(!calls.is_empty());
    assert!(
        calls
            .iter()
            .all(|(_, token, actual)| token == "valid-token" && *actual == Some(deadline)),
        "{calls:?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(phase, _, _)| *phase == "auth")
            .count(),
        5
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(phase, _, _)| *phase == "bind")
            .count(),
        5
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(phase, _, _)| *phase == "query")
            .count(),
        5
    );
}

#[tokio::test]
async fn latest_second_segment_errors_keep_status_and_original_principal_for_get_and_head() {
    let provider = Arc::new(FileAssetProvider {
        user_id: UserId::new(),
        dummy_video_path: PathBuf::new(),
        observed: Mutex::new(Vec::new()),
    });
    let app = media_server::routes(provider.clone());
    let started = Instant::now();
    let deadline = started + Duration::from_secs(30);
    for prefix in ["", "/emby"] {
        for method in ["GET", "HEAD"] {
            for (error, expected) in [
                ("unauthorized", StatusCode::UNAUTHORIZED),
                ("internal", StatusCode::INTERNAL_SERVER_ERROR),
                ("unavailable", StatusCode::SERVICE_UNAVAILABLE),
            ] {
                provider.observed.lock().unwrap().clear();
                let mut request = Request::builder()
                    .method(method)
                    .uri(format!(
                        "{prefix}/Users/me/Items/Latest?ParentId=latest-second-{error}"
                    ))
                    .header("authorization", "Bearer valid-token")
                    .body(Body::empty())
                    .unwrap();
                request
                    .extensions_mut()
                    .insert(media_server::provider::QueryTiming { started, deadline });
                let response = app.clone().oneshot(request).await.unwrap();
                assert_eq!(response.status(), expected, "{prefix} {method} {error}");
                let calls = provider.observed.lock().unwrap();
                assert_eq!(
                    calls
                        .iter()
                        .filter(|(phase, _, _)| *phase == "query")
                        .count(),
                    2
                );
                assert!(
                    calls
                        .iter()
                        .all(|(_, token, actual)| token == "valid-token"
                            && *actual == Some(deadline)),
                    "{calls:?}"
                );
            }
        }
    }
}

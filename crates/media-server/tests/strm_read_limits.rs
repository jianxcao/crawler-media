#[path = "support/mod.rs"]
mod support;
use support::fixture_provider;

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::StatusCode;
use domain::UserId;
use media_server::provider::{
    AuthenticateError, MediaItemSnapshot, MediaServerProvider, PlaybackClientInfo, PlaybackEvent,
    PlaybackEventIdentity, ServerLibrary, ServerUser,
};
use media_server::routes;
use tower::ServiceExt;

#[derive(Default)]
struct TestStrmProvider {
    strm_path: PathBuf,
}

#[async_trait::async_trait]
impl MediaServerProvider for TestStrmProvider {
    async fn resumable_items_page(
        &self,
        user_id: domain::UserId,
        parent_id: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<media_server::provider::ServerItemPage, String> {
        fixture_provider::resume_items(self, user_id, parent_id, offset, limit).await
    }

    async fn next_up_items(
        &self,
        user_id: domain::UserId,
        series_id: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<media_server::provider::ServerItemPage, String> {
        fixture_provider::next_up_items(self, user_id, series_id, offset, limit).await
    }
    async fn query_latest_items(
        &self,
        _principal: media_server::provider::QueryPrincipal,
        _query: media_server::provider::ServerItemQuery,
    ) -> Result<Vec<MediaItemSnapshot>, media_server::provider::QueryError> {
        Ok(Vec::new())
    }

    async fn query_visible_items(
        &self,
        principal: media_server::provider::QueryPrincipal,
        query: media_server::provider::ServerItemQuery,
    ) -> Result<media_server::provider::ServerItemPage, media_server::provider::QueryError> {
        fixture_provider::select_items(self, principal.user_id, query)
            .await
            .map_err(Into::into)
    }

    async fn issue_play_session(
        &self,
        _user_id: UserId,
        _device_id: &str,
        _item_id: &str,
    ) -> Result<String, String> {
        Ok(uuid::Uuid::new_v4().to_string())
    }
    fn server_id(&self) -> String {
        "test-server".to_string()
    }
    async fn authenticate_password(
        &self,
        _username: &str,
        _password: &str,
    ) -> Result<Option<(String, ServerUser)>, AuthenticateError> {
        Ok(None)
    }
    async fn user_id_by_token(
        &self,
        _token: &str,
        _deadline: Option<std::time::Instant>,
    ) -> Result<Option<UserId>, media_server::provider::QueryError> {
        Ok(Some(UserId::new()))
    }
    async fn get_user_for_query(
        &self,
        principal: media_server::provider::QueryPrincipal,
    ) -> Result<Option<ServerUser>, media_server::provider::QueryError> {
        self.get_user(principal.user_id)
            .await
            .map_err(media_server::provider::QueryError::Internal)
    }
    async fn list_visible_libraries_for_query(
        &self,
        principal: media_server::provider::QueryPrincipal,
    ) -> Result<Vec<ServerLibrary>, media_server::provider::QueryError> {
        self.list_visible_libraries(principal.user_id)
            .await
            .map_err(media_server::provider::QueryError::Internal)
    }
    async fn get_user(&self, id: UserId) -> Result<Option<ServerUser>, String> {
        Ok(Some(ServerUser {
            id,
            login: "admin".into(),
            is_admin: true,
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
    async fn resolve_stream_source(
        &self,
        _user: UserId,
        _id: &str,
    ) -> Result<Option<(std::path::PathBuf, bool)>, String> {
        Ok(Some((self.strm_path.clone(), true)))
    }
    async fn set_user_marks(
        &self,
        _user: UserId,
        _id: &str,
        _played: Option<bool>,
        _fav: Option<bool>,
    ) -> Result<bool, media_server::provider::UserMarksError> {
        Ok(true)
    }
    async fn report_playback_event(
        &self,
        _user: UserId,
        _id: &str,
        _pos: i64,
        _paused: bool,
        _client: PlaybackClientInfo,
        _event: PlaybackEvent,
        _identity: PlaybackEventIdentity,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test]
async fn strm_file_over_64kib_returns_bad_gateway() {
    let tmp = tempfile::tempdir().unwrap();
    let strm_path = tmp.path().join("large.strm");

    // Write 65537 bytes (> 64KiB = 65536)
    let large_bytes = vec![b'a'; 65_537];
    std::fs::write(&strm_path, large_bytes).unwrap();

    let provider = Arc::new(TestStrmProvider { strm_path });
    let app = routes::media_routes(provider);

    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/Videos/item1/stream")
        .header("Authorization", "MediaBrowser Token=\"test\"")
        .body(axum::body::Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

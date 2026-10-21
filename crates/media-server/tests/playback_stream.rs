use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::UserId;
use media_server::provider::{
    AuthenticateError, MediaItemSnapshot, MediaServerProvider, ServerLibrary, ServerUser,
};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

struct DirectStreamProvider {
    user_id: UserId,
    strm_path: PathBuf,
}

#[async_trait::async_trait]
impl MediaServerProvider for DirectStreamProvider {
    async fn issue_play_session(
        &self,
        _user_id: UserId,
        _device_id: &str,
        _item_id: &str,
    ) -> Result<String, String> {
        Ok(uuid::Uuid::new_v4().to_string())
    }
    fn server_id(&self) -> String {
        "test-server".into()
    }

    async fn authenticate_password(
        &self,
        _username: &str,
        _password: &str,
    ) -> Result<Option<(String, ServerUser)>, AuthenticateError> {
        Ok(None)
    }

    async fn user_id_by_token(&self, token: &str) -> Result<Option<UserId>, String> {
        Ok((token == "play-token").then_some(self.user_id))
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
        _user_id: UserId,
        id: &str,
    ) -> Result<Option<(PathBuf, bool)>, String> {
        Ok((id == "episode").then(|| (self.strm_path.clone(), true)))
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

    async fn update_progress(
        &self,
        _user_id: UserId,
        _id: &str,
        _position_ms: i64,
        _paused: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn is_device_revoked(&self, _user_id: UserId, device_id: &str) -> Result<bool, String> {
        Ok(device_id == "banned-device")
    }
}

#[tokio::test]
async fn strm_playback_redirects_directly_to_the_source_url() {
    let tmp = tempfile::tempdir().unwrap();
    let strm_path = tmp.path().join("episode.strm");
    std::fs::write(&strm_path, "https://cdn.example/episode.mkv?token=secret\n").unwrap();
    let provider = Arc::new(DirectStreamProvider {
        user_id: UserId::new(),
        strm_path,
    });
    let app = media_server::media_routes(provider);
    let request = Request::get("/Videos/episode/stream")
        .header("authorization", "Bearer play-token")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers().get("location").unwrap(),
        "https://cdn.example/episode.mkv?token=secret"
    );
}

#[tokio::test]
async fn revoked_device_stream_request_is_forbidden() {
    let tmp = tempfile::tempdir().unwrap();
    let strm_path = tmp.path().join("episode.strm");
    std::fs::write(&strm_path, "https://cdn.example/episode.mkv\n").unwrap();
    let provider = Arc::new(DirectStreamProvider {
        user_id: UserId::new(),
        strm_path,
    });
    let app = media_server::media_routes(provider);
    let request = Request::get("/Videos/episode/stream?DeviceId=banned-device")
        .header("authorization", "Bearer play-token")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn selected_version_is_the_streamed_version() {
    let tmp = tempfile::tempdir().unwrap();
    let file_a_path = tmp.path().join("movie_a.mkv");
    let file_b_path = tmp.path().join("movie_b.mkv");
    std::fs::write(&file_a_path, b"AAAA_movie_content_extra").unwrap();
    std::fs::write(&file_b_path, b"BBBB_movie_content_extra").unwrap();

    let provider = Arc::new(MultiVersionStreamProvider {
        user_id: UserId::new(),
        file_a_path,
        file_b_path,
    });
    let app = media_server::media_routes(provider);

    // Request stream for file B specifically via MediaSourceId / file_id
    let request = Request::get("/Videos/movie_item/stream?MediaSourceId=file_b")
        .header("authorization", "Bearer play-token")
        .header("range", "bytes=0-3")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    // Range bytes must equal the first four bytes of selected source B ("BBBB")
    assert_eq!(&bytes[..], b"BBBB");
}

struct MultiVersionStreamProvider {
    user_id: UserId,
    file_a_path: PathBuf,
    file_b_path: PathBuf,
}

#[async_trait::async_trait]
impl MediaServerProvider for MultiVersionStreamProvider {
    fn server_id(&self) -> String {
        "test-multi-version".into()
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
    async fn user_id_by_token(&self, token: &str) -> Result<Option<UserId>, String> {
        Ok((token == "play-token").then_some(self.user_id))
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
        _id: &str,
    ) -> Result<Option<(PathBuf, bool)>, String> {
        Ok(Some((self.file_a_path.clone(), false)))
    }
    async fn resolve_stream_source_bound(
        &self,
        _user_id: UserId,
        _id: &str,
        media_source_id: Option<&str>,
        _play_session_id: Option<&str>,
        _device_id: Option<&str>,
    ) -> Result<Option<(PathBuf, bool)>, String> {
        if media_source_id == Some("file_b") {
            Ok(Some((self.file_b_path.clone(), false)))
        } else {
            Ok(Some((self.file_a_path.clone(), false)))
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
    async fn update_progress(
        &self,
        _user_id: UserId,
        _id: &str,
        _position_ms: i64,
        _paused: bool,
    ) -> Result<(), String> {
        Ok(())
    }
    async fn is_device_revoked(&self, _user_id: UserId, _device_id: &str) -> Result<bool, String> {
        Ok(false)
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

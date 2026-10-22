use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::UserId;
use media_server::provider::{
    AuthenticateError, MediaItemSnapshot, MediaServerProvider, PlaybackClientInfo, PlaybackEvent,
    PlaybackEventIdentity, ServerLibrary, ServerUser,
};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

struct FileAssetProvider {
    user_id: UserId,
    dummy_video_path: PathBuf,
}

#[async_trait::async_trait]
impl MediaServerProvider for FileAssetProvider {
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
    async fn user_id_by_token(&self, token: &str) -> Result<Option<UserId>, String> {
        Ok((token == "valid-token").then_some(self.user_id))
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
async fn file_assets_require_authentication() {
    let tmp = tempfile::tempdir().unwrap();
    let dummy_video_path = tmp.path().join("dummy.mkv");
    let provider = Arc::new(FileAssetProvider {
        user_id: UserId::new(),
        dummy_video_path,
    });
    let app = media_server::media_routes(provider);

    let req_fonts = Request::get("/playback/files/file-1/fonts")
        .body(Body::empty())
        .unwrap();
    let res_fonts = app.clone().oneshot(req_fonts).await.unwrap();
    assert_eq!(res_fonts.status(), StatusCode::UNAUTHORIZED);

    let req_trickplay = Request::get("/playback/files/file-1/trickplay")
        .body(Body::empty())
        .unwrap();
    let res_trickplay = app.oneshot(req_trickplay).await.unwrap();
    assert_eq!(res_trickplay.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn missing_file_assets_return_404_not_empty_200() {
    let tmp = tempfile::tempdir().unwrap();
    let dummy_video_path = tmp.path().join("dummy.mkv");
    let provider = Arc::new(FileAssetProvider {
        user_id: UserId::new(),
        dummy_video_path,
    });
    let app = media_server::media_routes(provider);

    let req_fonts = Request::get("/playback/files/file-1/fonts?token=valid-token")
        .body(Body::empty())
        .unwrap();
    let res_fonts = app.clone().oneshot(req_fonts).await.unwrap();
    assert_eq!(res_fonts.status(), StatusCode::NOT_FOUND);

    let req_trickplay = Request::get("/playback/files/file-1/trickplay?token=valid-token")
        .body(Body::empty())
        .unwrap();
    let res_trickplay = app.oneshot(req_trickplay).await.unwrap();
    assert_eq!(res_trickplay.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn present_file_assets_stream_real_bytes_with_content_type() {
    let tmp = tempfile::tempdir().unwrap();
    let dummy_video_path = tmp.path().join("dummy.mkv");
    std::fs::write(&dummy_video_path, b"video content").unwrap();

    let trickplay_img = tmp.path().join("dummy.trickplay.jpg");
    std::fs::write(&trickplay_img, b"\xFF\xD8\xFF\xE0JPEG_TRICKPLAY_BYTES").unwrap();

    let fonts_dir = tmp.path().join("dummy.fonts");
    std::fs::create_dir_all(&fonts_dir).unwrap();
    std::fs::write(fonts_dir.join("font1.ttf"), b"FONT1").unwrap();

    let provider = Arc::new(FileAssetProvider {
        user_id: UserId::new(),
        dummy_video_path,
    });
    let app = media_server::media_routes(provider);

    // 1. Trickplay returns real image bytes and Content-Type: image/jpeg
    let req_trickplay = Request::get("/playback/files/present-file/trickplay?token=valid-token")
        .body(Body::empty())
        .unwrap();
    let res_trickplay = app.clone().oneshot(req_trickplay).await.unwrap();
    assert_eq!(res_trickplay.status(), StatusCode::OK);
    assert_eq!(
        res_trickplay.headers().get("content-type").unwrap(),
        "image/jpeg"
    );
    let bytes = axum::body::to_bytes(res_trickplay.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"\xFF\xD8\xFF\xE0JPEG_TRICKPLAY_BYTES");

    // 2. Fonts returns list of embedded fonts
    let req_fonts = Request::get("/playback/files/present-file/fonts?token=valid-token")
        .body(Body::empty())
        .unwrap();
    let res_fonts = app.oneshot(req_fonts).await.unwrap();
    assert_eq!(res_fonts.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(res_fonts.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["fonts"][0], "font1.ttf");
}

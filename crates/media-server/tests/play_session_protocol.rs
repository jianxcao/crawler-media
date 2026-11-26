#[path = "support/mod.rs"]
mod support;
use support::fixture_provider;

use std::sync::Arc;

use axum::http::StatusCode;
use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    Media, MediaId, MediaKind, QualitySource, TypedItemId, UnitKey, UserId,
};
use media_server::provider::{
    AuthenticateError, MediaItemMetadata, MediaItemSnapshot, MediaServerProvider,
    PlaybackClientInfo, PlaybackEvent, PlaybackEventIdentity, ServerLibrary, ServerUser,
};
use media_server::routes;
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Default)]
struct TestProvider {
    series_snapshot: Option<MediaItemSnapshot>,
    movie_snapshot: Option<MediaItemSnapshot>,
}

#[async_trait::async_trait]
impl MediaServerProvider for TestProvider {
    async fn resumable_items_page(
        &self,
        user_id: domain::UserId,
        parent_id: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<media_server::provider::ServerItemPage, media_server::provider::QueryError> {
        fixture_provider::resume_items(self, user_id, parent_id, offset, limit).await
    }

    async fn next_up_items(
        &self,
        user_id: domain::UserId,
        series_id: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<media_server::provider::ServerItemPage, media_server::provider::QueryError> {
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
        _snapshot: &media_server::provider::MediaItemSnapshot,
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
    async fn revoke_token(&self, _token: &str) -> Result<(), media_server::provider::QueryError> {
        Ok(())
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
        id: &str,
    ) -> Result<Option<MediaItemSnapshot>, String> {
        if let Some(series) = &self.series_snapshot {
            let wire = TypedItemId::Work(series.media.id).to_wire_string();
            if id == wire || id == series.media.id.to_string() {
                return Ok(Some(series.clone()));
            }
        }
        if let Some(movie) = &self.movie_snapshot {
            let wire = TypedItemId::Work(movie.media.id).to_wire_string();
            if id == wire || id == movie.media.id.to_string() {
                return Ok(Some(movie.clone()));
            }
        }
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
        Ok(Some((std::path::PathBuf::from("/movies/test.mkv"), false)))
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
        _pos: Option<i64>,
        _paused: bool,
        _client: PlaybackClientInfo,
        _event: PlaybackEvent,
        _identity: PlaybackEventIdentity,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test]
async fn sequence_and_id_length_validation() {
    let provider = Arc::new(TestProvider::default());
    let app = routes::routes(provider);

    // Negative sequence returns 400
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/Sessions/Playing/Progress")
        .header("Authorization", "MediaBrowser Token=\"test\"")
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            serde_json::to_vec(&json!({
                "ItemId": "item1",
                "PositionTicks": 100000,
                "EventSequence": -1,
            }))
            .unwrap(),
        ))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Too long PlaySessionId returns 400
    let long_id = "a".repeat(129);
    let req2 = axum::http::Request::builder()
        .method("POST")
        .uri("/Sessions/Playing/Progress")
        .header("Authorization", "MediaBrowser Token=\"test\"")
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            serde_json::to_vec(&json!({
                "ItemId": "item1",
                "PositionTicks": 100000,
                "PlaySessionId": long_id,
            }))
            .unwrap(),
        ))
        .unwrap();

    let resp2 = app.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::BAD_REQUEST);
}

fn snapshot_with_subtitle(
    kind: MediaKind,
    is_series: bool,
    unit: Option<UnitKey>,
) -> MediaItemSnapshot {
    let media_id = MediaId::new();
    let mut tracks = library::Tracks::default();
    tracks.subtitles.push(library::SubtitleTrack {
        stream_index: Some(0),
        codec: Some("srt".into()),
        ..Default::default()
    });
    let file = LibraryFile {
        file_id: LedgerId::new(),
        library_id: LibraryId::new(),
        root_id: "root-1".into(),
        media_id,
        kind,
        path: std::path::PathBuf::from("/media/video.mkv"),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(128),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 1,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    MediaItemSnapshot {
        file,
        unit,
        media: Media {
            id: media_id,
            kind,
            title: "Fixture".into(),
            year: Some(2024),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        },
        ticks: 0,
        play_count: 0,
        played: false,
        unplayed_item_count: None,
        parent_id: "lib-1".into(),
        is_series,
        chapters: Vec::new(),
        intro_start_ms: None,
        intro_end_ms: None,
        outro_start_ms: None,
        outro_end_ms: None,
        metadata: MediaItemMetadata {
            tracks,
            ..Default::default()
        },
    }
}

#[tokio::test]
async fn playback_info_uses_concrete_unit_id_for_series_and_work_id_for_movie() {
    let series_snap = snapshot_with_subtitle(MediaKind::Tv, true, Some(UnitKey::new(1, 2)));
    let movie_snap = snapshot_with_subtitle(MediaKind::Movie, false, None);
    let series_id = series_snap.media.id;
    let movie_id = movie_snap.media.id;
    let app = routes::routes(Arc::new(TestProvider {
        series_snapshot: Some(series_snap),
        movie_snapshot: Some(movie_snap),
    }));

    let series_wire = TypedItemId::Work(series_id).to_wire_string();
    let series_req = axum::http::Request::builder()
        .method("POST")
        .uri(format!("/Items/{series_wire}/PlaybackInfo"))
        .header("Authorization", "MediaBrowser Token=\"test\"")
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from("{}"))
        .unwrap();
    let series_resp = app.clone().oneshot(series_req).await.unwrap();
    assert_eq!(series_resp.status(), StatusCode::OK);
    let series_json: Value = serde_json::from_slice(
        &axum::body::to_bytes(series_resp.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let expected_unit = TypedItemId::Unit {
        media: series_id,
        key: UnitKey::new(1, 2),
    }
    .to_wire_string();
    let delivery_url = series_json["MediaSources"][0]["MediaStreams"][0]["DeliveryUrl"]
        .as_str()
        .unwrap();
    assert!(
        delivery_url.starts_with(&format!("/Videos/{expected_unit}/Subtitles/")),
        "expected unit protocol id in DeliveryUrl, got {delivery_url}"
    );

    let movie_req = axum::http::Request::builder()
        .method("POST")
        .uri(format!("/Items/{movie_id}/PlaybackInfo"))
        .header("Authorization", "MediaBrowser Token=\"test\"")
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from("{}"))
        .unwrap();
    let movie_resp = app.oneshot(movie_req).await.unwrap();
    assert_eq!(movie_resp.status(), StatusCode::OK);
    let movie_json: Value = serde_json::from_slice(
        &axum::body::to_bytes(movie_resp.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let expected_work = TypedItemId::Work(movie_id).to_wire_string();
    let movie_delivery = movie_json["MediaSources"][0]["MediaStreams"][0]["DeliveryUrl"]
        .as_str()
        .unwrap();
    assert!(
        movie_delivery.starts_with(&format!("/Videos/{expected_work}/Subtitles/")),
        "expected work protocol id in DeliveryUrl, got {movie_delivery}"
    );
}

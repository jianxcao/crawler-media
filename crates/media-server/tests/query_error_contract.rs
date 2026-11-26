#[path = "support/mod.rs"]
mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId,
    Media, MediaId, MediaKind, QualitySource, UserId,
};
use media_server::provider::{
    AuthenticateError, EntityKind, EntityPage, ItemCounts, MediaItemMetadata, MediaItemSnapshot,
    MediaServerProvider, PlaybackClientInfo, PlaybackEvent, PlaybackEventIdentity, QueryError,
    Season, ServerLibrary, ServerUser,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tower::ServiceExt;

struct FileAssetProvider {
    user_id: UserId,
    dummy_video_path: PathBuf,
    observed: Mutex<Vec<(&'static str, String, Option<Instant>)>>,
    forced_error: Mutex<Option<&'static str>>,
}

impl FileAssetProvider {
    fn new(user_id: UserId) -> Self {
        Self {
            user_id,
            dummy_video_path: PathBuf::new(),
            observed: Mutex::new(Vec::new()),
            forced_error: Mutex::new(None),
        }
    }

    fn marker_error(&self, marker: Option<&str>) -> QueryError {
        let key = marker
            .filter(|m| matches!(*m, "unavailable" | "unauthorized" | "internal"))
            .or_else(|| *self.forced_error.lock().unwrap());
        match key {
            Some("unauthorized") => QueryError::Unauthorized,
            Some("internal") => QueryError::Internal("fixture failure".into()),
            _ => QueryError::Unavailable,
        }
    }

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
    async fn resumable_items_page(
        &self,
        _user_id: domain::UserId,
        parent_id: Option<&str>,
        _offset: usize,
        _limit: usize,
    ) -> Result<media_server::provider::ServerItemPage, QueryError> {
        Err(self.marker_error(parent_id))
    }

    async fn next_up_items(
        &self,
        _user_id: domain::UserId,
        series_id: Option<&str>,
        _offset: usize,
        _limit: usize,
    ) -> Result<media_server::provider::ServerItemPage, QueryError> {
        Err(self.marker_error(series_id))
    }

    async fn query_entities(
        &self,
        _user: UserId,
        _kind: EntityKind,
        _parent: Option<&str>,
        search: Option<&str>,
        _offset: usize,
        _limit: usize,
    ) -> Result<EntityPage, QueryError> {
        Err(self.marker_error(search))
    }

    async fn query_item_counts(&self, _user: UserId) -> Result<ItemCounts, QueryError> {
        Err(self.marker_error(None))
    }

    async fn query_seasons(
        &self,
        _user: UserId,
        media: MediaId,
    ) -> Result<Vec<Season>, QueryError> {
        let marker = match media.to_string().as_str() {
            "00000000-0000-0000-0000-000000000002" => Some("unauthorized"),
            "00000000-0000-0000-0000-000000000003" => Some("internal"),
            _ => Some("unavailable"),
        };
        Err(self.marker_error(marker))
    }

    async fn search_hints_page(
        &self,
        _user: UserId,
        term: &str,
        _offset: usize,
        _limit: usize,
    ) -> Result<media_server::provider::ServerItemPage, QueryError> {
        Err(self.marker_error(Some(term)))
    }

    async fn resolve_single_item_for_query(
        &self,
        _user_id: UserId,
        id: &str,
    ) -> Result<Option<MediaItemSnapshot>, QueryError> {
        match id {
            "resolve-unavailable" => Err(QueryError::Unavailable),
            "resolve-unauthorized" => Err(QueryError::Unauthorized),
            "resolve-internal" => Err(QueryError::Internal("fixture".into())),
            _ => Ok(series_fixture_snapshot(id)),
        }
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
    async fn revoke_token(&self, _token: &str) -> Result<(), QueryError> {
        Ok(())
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
    ) -> Result<bool, media_server::provider::UserMarksError> {
        Ok(true)
    }
    async fn report_playback_event(
        &self,
        _user_id: UserId,
        _id: &str,
        _position_ms: Option<i64>,
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
        _snapshot: &media_server::provider::MediaItemSnapshot,
    ) -> Result<String, String> {
        Ok(uuid::Uuid::new_v4().to_string())
    }
}

#[tokio::test]
async fn typed_query_errors_keep_http_status_on_catalog_and_episode_routes() {
    let provider = Arc::new(FileAssetProvider::new(UserId::new()));
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
    let provider = Arc::new(FileAssetProvider::new(UserId::new()));
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
    let provider = Arc::new(FileAssetProvider::new(UserId::new()));
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
    let provider = Arc::new(FileAssetProvider::new(UserId::new()));
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
    let provider = Arc::new(FileAssetProvider::new(UserId::new()));
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
    let provider = Arc::new(FileAssetProvider::new(UserId::new()));
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

fn series_fixture_snapshot(id: &str) -> Option<MediaItemSnapshot> {
    let media_id = match id {
        "w1_00000000000000000000000000000001" => "00000000-0000-0000-0000-000000000001",
        "w1_00000000000000000000000000000002" => "00000000-0000-0000-0000-000000000002",
        "w1_00000000000000000000000000000003" => "00000000-0000-0000-0000-000000000003",
        _ => return None,
    }
    .parse()
    .ok()?;
    let file = LibraryFile {
        file_id: LedgerId::new(),
        library_id: LibraryId::new(),
        root_id: "root".into(),
        media_id,
        kind: MediaKind::Tv,
        path: PathBuf::from("/tv/show.mkv"),
        version: FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: Some(100),
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
    Some(MediaItemSnapshot {
        file,
        unit: None,
        media: Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Show".into(),
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
        parent_id: "lib".into(),
        is_series: true,
        chapters: Vec::new(),
        intro_start_ms: None,
        intro_end_ms: None,
        outro_start_ms: None,
        outro_end_ms: None,
        metadata: MediaItemMetadata::default(),
    })
}

#[tokio::test]
async fn projection_and_continuation_routes_preserve_typed_query_errors() {
    let provider = Arc::new(FileAssetProvider::new(UserId::new()));
    let app = media_server::routes(provider.clone());
    let cases = [
        (
            "/Genres?SearchTerm=",
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
        ),
        (
            "/emby/Genres?SearchTerm=unauthorized",
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            "/Studios?SearchTerm=internal",
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        ),
        (
            "/emby/Studios?SearchTerm=unavailable",
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
        ),
        (
            "/Items/Counts",
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
        ),
        (
            "/Users/me/Items/Counts",
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            "/Users/me/Items/Counts",
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        ),
        (
            "/Shows/w1_00000000000000000000000000000001/Seasons",
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
        ),
        (
            "/Shows/w1_00000000000000000000000000000002/Seasons",
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            "/Shows/w1_00000000000000000000000000000003/Seasons",
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        ),
        (
            "/Shows/resolve-unavailable/Seasons",
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
        ),
        (
            "/Shows/resolve-unauthorized/Seasons",
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            "/Shows/resolve-internal/Seasons",
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        ),
        (
            "/Search/Hints?SearchTerm=test",
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
        ),
        (
            "/Search/Hints?SearchTerm=unauthorized",
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            "/Search/Hints?SearchTerm=internal",
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        ),
        (
            "/Users/Me/Items/Resume",
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
        ),
        (
            "/Users/Me/Items/Resume?ParentId=unauthorized",
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            "/Shows/NextUp",
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
        ),
        (
            "/Shows/NextUp?SeriesId=internal",
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        ),
    ];
    for (uri, expected, marker) in cases {
        *provider.forced_error.lock().unwrap() = Some(marker);
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
        assert_eq!(response.status(), expected, "{uri} ({marker})");
    }
}

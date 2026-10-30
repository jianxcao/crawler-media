use domain::{LibraryFile, Media, MediaKind, TypedItemId, UnitKey, UserId};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

mod entities;
pub mod query;
pub use entities::{Entity, EntityKind, EntityPage, ItemCounts, Season};

pub use query::{ServerItemPage, ServerItemQuery};

#[derive(Clone, Debug)]
pub struct ServerLibrary {
    pub id: String,
    pub name: String,
    pub kind: MediaKind,
    pub exclude_from_home: bool,
    pub root_paths: Vec<PathBuf>,
    pub cover_path: Option<String>,
    pub cover_tag: String,
}

#[derive(Clone, Debug)]
pub struct ServerUser {
    pub id: UserId,
    pub login: String,
    pub is_admin: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackEvent {
    Playing,
    Progress,
    Stopped,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackClientInfo {
    pub device_id: String,
    pub client: Option<String>,
    pub device_name: Option<String>,
    pub client_version: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackEventIdentity {
    pub play_session_id: Option<String>,
    pub event_sequence: Option<i64>,
}

/// One library entry as the protocol layer sees it: the selected source file,
/// the row view its artwork/NFO helpers consume, and the media it belongs to.
#[derive(Clone, Debug)]
pub struct MediaItemSnapshot {
    /// The published, present source file this entry plays. Public item
    /// identity is the logical target, never this file id.
    pub file: LibraryFile,
    pub unit: Option<UnitKey>,
    pub media: Media,
    pub ticks: i64,
    pub play_count: i64,
    pub played: bool,
    pub unplayed_item_count: Option<usize>,
    pub parent_id: String,
    pub is_series: bool,
    pub chapters: Vec<library::ChapterMarker>,
    pub intro_start_ms: Option<i64>,
    pub intro_end_ms: Option<i64>,
    pub outro_start_ms: Option<i64>,
    pub outro_end_ms: Option<i64>,
    pub metadata: MediaItemMetadata,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UserItemData {
    pub key: String,
    pub item_id: String,
    pub playback_position_ticks: i64,
    pub play_count: i64,
    pub played: bool,
    pub is_favorite: bool,
    pub unplayed_item_count: Option<usize>,
    pub played_percentage: Option<f64>,
}

impl UserItemData {
    pub fn json_value(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "PlaybackPositionTicks": self.playback_position_ticks,
            "PlayCount": self.play_count,
            "IsFavorite": self.is_favorite,
            "Played": self.played,
            "Key": self.key,
            "ItemId": self.item_id,
        });
        if let Some(count) = self.unplayed_item_count {
            value["UnplayedItemCount"] = serde_json::json!(count);
        }
        if let Some(percentage) = self.played_percentage {
            value["PlayedPercentage"] = serde_json::json!(percentage);
        }
        value
    }
}

impl MediaItemSnapshot {
    /// Logical identity of this entry: its work for movies, videos and series,
    /// and the exact unit for a TV episode. This is what the protocol exposes as
    /// the item id — the physical file id is never an item identity.
    pub fn item_identity(&self) -> TypedItemId {
        match (self.media.kind, self.is_series) {
            (MediaKind::Tv, false) => match self.unit {
                Some(key) => TypedItemId::Unit {
                    media: self.media.id,
                    key,
                },
                // Degenerate TV row without unit coverage: the whole work unit,
                // never a fabricated `(0, 0)` coordinate.
                None => TypedItemId::Unit {
                    media: self.media.id,
                    key: UnitKey::WHOLE,
                },
            },
            _ => TypedItemId::Work(self.media.id),
        }
    }

    /// Public protocol id of this entry (`w1_…` work, `u1_…` unit).
    pub fn protocol_id(&self) -> String {
        self.item_identity().to_wire_string()
    }

    pub fn user_item_data(&self) -> UserItemData {
        let item_id = self.protocol_id();
        let playback_position_ticks = if self.is_series { 0 } else { self.ticks };
        let played_percentage = if let Some(unplayed) = self.unplayed_item_count {
            self.metadata
                .child_count
                .filter(|total| *total > 0)
                .map(|total| total.saturating_sub(unplayed) as f64 / total as f64 * 100.0)
        } else if playback_position_ticks > 0 {
            self.metadata
                .runtime_ticks
                .filter(|runtime| *runtime > 0)
                .map(|runtime| playback_position_ticks as f64 / runtime as f64 * 100.0)
        } else {
            None
        };
        UserItemData {
            key: item_id.clone(),
            item_id,
            playback_position_ticks,
            play_count: if self.is_series { 0 } else { self.play_count },
            played: self.played,
            is_favorite: self.metadata.is_favorite,
            unplayed_item_count: self.unplayed_item_count,
            played_percentage,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MediaItemMetadata {
    pub overview: Option<String>,
    pub community_rating: Option<f64>,
    pub vote_count: Option<u64>,
    pub genres: Vec<String>,
    pub production_year: Option<i32>,
    pub original_title: Option<String>,
    pub premiere_date: Option<String>,
    pub date_created: Option<String>,
    pub end_date: Option<String>,
    pub official_rating: Option<String>,
    pub status: Option<String>,
    pub original_language: Option<String>,
    pub taglines: Vec<String>,
    pub studios: Vec<String>,
    pub production_locations: Vec<String>,
    pub number_of_episodes: Option<u32>,
    pub runtime_ticks: Option<i64>,
    pub provider_ids: BTreeMap<String, String>,
    pub episode_name: Option<String>,
    pub people: Vec<MediaPerson>,
    pub is_favorite: bool,
    pub resume_updated_at: i64,
    pub child_count: Option<usize>,
    pub season_count: Option<usize>,
    pub has_primary_image: bool,
    pub has_backdrop_image: bool,
    pub primary_image_tag: Option<String>,
    pub primary_image_url: Option<String>,
    pub tracks: library::Tracks,
}

#[derive(Clone, Debug, Default)]
pub struct MediaPerson {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    pub person_type: String,
    pub tmdb_id: Option<String>,
    pub primary_image_url: Option<String>,
}

/// 密码登录失败原因。`Busy` 映射 503，`ReadOnly` 映射 409，`Internal` 映射 500；
/// 内部故障绝不能退化成“密码错误”(401) 返回给客户端。
#[derive(Debug, thiserror::Error)]
pub enum AuthenticateError {
    #[error("authentication service busy")]
    Busy,
    #[error("instance is read-only")]
    ReadOnly,
    #[error("{0}")]
    Internal(String),
}

/// 字幕交付失败原因。协议层据此映射 503/504/409/500；`Ok(None)` 表示没有
/// 可见的字幕轨或文件（映射 404）。
#[derive(Debug, thiserror::Error)]
pub enum SubtitleDeliveryError {
    #[error("subtitle delivery unsupported")]
    Unsupported,
    #[error("subtitle service busy")]
    Busy,
    #[error("subtitle operation timed out")]
    Timeout,
    #[error("Library no longer available")]
    Cancelled,
    #[error("subtitle delivery failed: {0}")]
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum UserMarksError {
    #[error("user marks temporarily unavailable")]
    Unavailable,
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Internal(String),
}

impl UserMarksError {
    pub fn status_code(&self) -> axum::http::StatusCode {
        match self {
            Self::Unavailable => axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Self::Conflict(_) => axum::http::StatusCode::CONFLICT,
            Self::Internal(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("unauthorized query")]
    Unauthorized,
    #[error("query temporarily unavailable")]
    Unavailable,
    #[error("{0}")]
    Internal(String),
}
impl From<String> for QueryError {
    fn from(value: String) -> Self {
        Self::Internal(value)
    }
}
impl QueryError {
    pub fn status_code(&self) -> axum::http::StatusCode {
        match self {
            Self::Unauthorized => axum::http::StatusCode::UNAUTHORIZED,
            Self::Unavailable => axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Original request credential and one absolute budget for a covered query.
#[derive(Clone)]
pub struct QueryPrincipal {
    pub user_id: UserId,
    pub token: String,
    pub deadline: std::time::Instant,
}
impl std::fmt::Debug for QueryPrincipal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryPrincipal")
            .field("user_id", &self.user_id)
            .field("token", &"<redacted>")
            .field("deadline", &self.deadline)
            .finish()
    }
}

/// Timing is created before authentication; tests may inject a shorter deadline.
#[derive(Clone, Copy, Debug)]
pub struct QueryTiming {
    pub started: std::time::Instant,
    pub deadline: std::time::Instant,
}
/// Injected short timing used by Router tests; promoted only by a covered route layer.
#[derive(Clone, Copy, Debug)]
pub struct QueryTimingSeed(pub QueryTiming);

impl QueryTiming {
    pub fn start() -> Self {
        let started = std::time::Instant::now();
        Self {
            started,
            deadline: started + Duration::from_secs(30),
        }
    }
}

#[async_trait::async_trait]
pub trait MediaServerProvider: Send + Sync + 'static {
    fn server_id(&self) -> String;

    /// Optional actual HTTP encoding boundary used by the release harness.
    fn record_query_phase(&self, _phase: &'static str, _duration: Duration) {}

    async fn authenticate_password(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<(String, ServerUser)>, AuthenticateError>;

    async fn user_id_by_token(
        &self,
        token: &str,
        deadline: Option<std::time::Instant>,
    ) -> Result<Option<UserId>, QueryError>;

    /// 通过共享 single-flight 服务交付字幕轨。
    ///
    /// 默认不支持：实现方必须显式 override，**绝不默认回退到同步提取**
    /// （那会在 async worker 上阻塞）。`Ok(None)` 表示没有可见的字幕轨/文件。
    async fn deliver_subtitle(
        &self,
        _user_id: UserId,
        _item_id: &str,
        _index: u32,
        _wants_vtt: bool,
    ) -> Result<Option<library::SubtitlePayload>, SubtitleDeliveryError> {
        Err(SubtitleDeliveryError::Unsupported)
    }

    /// Persist credentials observed on authenticated protocol requests. Mock providers
    /// may omit persistence; production uses this binding for secure device revocation.
    async fn bind_playback_credential(
        &self,
        _user_id: UserId,
        _device_id: &str,
        _token: &str,
        _deadline: Option<std::time::Instant>,
    ) -> Result<(), QueryError> {
        Ok(())
    }

    async fn get_user(&self, user_id: UserId) -> Result<Option<ServerUser>, String>;
    /// Covered query route user-path lookup uses the same absolute budget.
    async fn get_user_for_query(
        &self,
        principal: QueryPrincipal,
    ) -> Result<Option<ServerUser>, QueryError>;

    /// Lists libraries visible specifically to this user
    async fn list_visible_libraries(&self, user_id: UserId) -> Result<Vec<ServerLibrary>, String>;
    /// Covered catalog parent classification uses the request budget.
    async fn list_visible_libraries_for_query(
        &self,
        principal: QueryPrincipal,
    ) -> Result<Vec<ServerLibrary>, QueryError>;

    /// These reads must select bounded projection facts, never enumerate snapshots.
    async fn query_entities(
        &self,
        _user: UserId,
        _kind: EntityKind,
        _parent: Option<&str>,
        _search: Option<&str>,
        _offset: usize,
        _limit: usize,
    ) -> Result<EntityPage, String> {
        Err("entity projection queries unsupported".into())
    }
    async fn query_item_counts(&self, _user: UserId) -> Result<ItemCounts, String> {
        Err("protocol count queries unsupported".into())
    }
    async fn query_seasons(
        &self,
        _user: UserId,
        _media: domain::MediaId,
    ) -> Result<Vec<Season>, String> {
        Err("season projection queries unsupported".into())
    }
    async fn search_hints_page(
        &self,
        _user: UserId,
        _term: &str,
        _offset: usize,
        _limit: usize,
    ) -> Result<ServerItemPage, String> {
        Err("bounded search hints unsupported".into())
    }
    async fn resolve_entity_image(
        &self,
        _user: Option<UserId>,
        _kind: EntityKind,
        _query: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        Err("entity image queries unsupported".into())
    }

    /// Lists media item snapshots visible to this user
    async fn list_visible_items(
        &self,
        user_id: UserId,
        parent_id: Option<&str>,
    ) -> Result<Vec<MediaItemSnapshot>, String>;
    /// Lists items visible to the user and owned by the requested library.
    /// Providers without library-aware filtering retain their existing listing behavior.
    async fn list_visible_items_in_library(
        &self,
        user_id: UserId,
        parent_id: Option<&str>,
        _library_id: &str,
    ) -> Result<Vec<MediaItemSnapshot>, String> {
        self.list_visible_items(user_id, parent_id).await
    }

    /// Complete selection must preserve typed availability and authorization errors.
    async fn query_visible_items(
        &self,
        principal: QueryPrincipal,
        query: ServerItemQuery,
    ) -> Result<ServerItemPage, QueryError>;

    /// Complete Latest composition, including both playback segments, must share
    /// one authorization/retry boundary and the original principal deadline.
    async fn query_latest_items(
        &self,
        principal: QueryPrincipal,
        query: ServerItemQuery,
    ) -> Result<Vec<MediaItemSnapshot>, QueryError>;

    /// NextUp：一次候选读 + 按 Media 分组选第一条未看，再整体分页。
    ///
    /// Required, not defaulted: a fallback built from full snapshots is unbounded
    /// and would silently become the behaviour of every new provider.
    async fn next_up_items(
        &self,
        user_id: UserId,
        series_id: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<ServerItemPage, String>;

    /// Select partial unplayed units within the parent scope before deduplication and paging.
    async fn resumable_items_page(
        &self,
        user_id: UserId,
        parent_id: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<ServerItemPage, String>;
    async fn resolve_single_item(
        &self,
        user_id: UserId,
        id: &str,
    ) -> Result<Option<MediaItemSnapshot>, String>;

    async fn resolve_user_item_data(
        &self,
        user_id: UserId,
        id: &str,
    ) -> Result<Option<UserItemData>, String> {
        Ok(self
            .resolve_single_item(user_id, id)
            .await?
            .map(|snapshot| snapshot.user_item_data()))
    }

    async fn enrich_visible_item(&self, item: MediaItemSnapshot) -> MediaItemSnapshot {
        item
    }

    async fn resolve_cover_bytes(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String>;

    async fn resolve_poster_bytes(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String>;

    async fn resolve_primary_image_url(
        &self,
        _user_id: Option<UserId>,
        _id: &str,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }

    async fn resolve_person_image_url(
        &self,
        _user_id: Option<UserId>,
        _name: &str,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }

    async fn deliver_person_image(
        &self,
        _user: Option<UserId>,
        _query: &str,
        _headers: axum::http::HeaderMap,
    ) -> Result<Option<axum::response::Response>, String> {
        Err("person image delivery unsupported".into())
    }

    async fn resolve_person(
        &self,
        _user: UserId,
        _query: &str,
    ) -> Result<Option<MediaPerson>, String> {
        Err("person projection queries unsupported".into())
    }

    async fn resolve_backdrop_bytes(
        &self,
        _user_id: Option<UserId>,
        _id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }

    async fn resolve_stream_source(
        &self,
        user_id: UserId,
        id: &str,
    ) -> Result<Option<(PathBuf, bool)>, String>;

    /// Resolve a stream source for a specific requested physical source (MediaSourceId)
    /// and/or play session id. Default implementation forwards to `resolve_stream_source`.
    async fn resolve_stream_source_bound(
        &self,
        user_id: UserId,
        id: &str,
        media_source_id: Option<&str>,
        _play_session_id: Option<&str>,
        _device_id: Option<&str>,
    ) -> Result<Option<(PathBuf, bool)>, String> {
        let _ = media_source_id;
        self.resolve_stream_source(user_id, id).await
    }

    /// A05: 检查特定设备是否已被用户撤销
    async fn is_device_revoked(&self, _user_id: UserId, _device_id: &str) -> Result<bool, String> {
        Ok(false)
    }

    async fn set_user_marks(
        &self,
        user_id: UserId,
        id: &str,
        played: Option<bool>,
        favorite: Option<bool>,
    ) -> Result<bool, UserMarksError>;

    async fn issue_play_session(
        &self,
        user_id: UserId,
        device_id: &str,
        item_id: &str,
    ) -> Result<String, String>;

    #[allow(clippy::too_many_arguments)]
    async fn report_playback_event(
        &self,
        user_id: UserId,
        id: &str,
        position_ms: i64,
        paused: bool,
        _client: PlaybackClientInfo,
        _event: PlaybackEvent,
        _identity: PlaybackEventIdentity,
    ) -> Result<(), String>;
}

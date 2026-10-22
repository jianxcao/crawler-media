use async_trait::async_trait;
use domain::{MediaKind, UserId};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use media_server::provider::{
    AuthenticateError, MediaItemSnapshot, MediaPerson, MediaServerProvider, PlaybackClientInfo,
    PlaybackEvent, ServerItemPage, ServerItemQuery, ServerLibrary, ServerUser,
    SubtitleDeliveryError,
};

use crate::subtitle_delivery::SubtitleServiceError;
use library::DeliveryError;

use crate::management::ApiState;
mod read_snapshot;
use store::PlaybackUnitSnapshot;

struct ServerReadSnapshot {
    ownership: store::LibraryOwnership,
    is_admin: bool,
    media: HashMap<domain::MediaId, domain::Media>,
    coverages: Vec<domain::FileCoverage>,
    units: Vec<PlaybackUnitSnapshot>,
    /// Published + present source facts per candidate file.
    files: HashMap<domain::LedgerId, domain::LibraryFile>,
}
mod item_metadata;
mod person_images;
mod playback;
mod stream_binding;
use item_metadata::{
    backdrop_path, item_metadata, local_primary_image, merge_catalog_metadata, primary_image_url,
};

mod enrichment;
pub(crate) mod items;
mod selection;
mod user_marks;
use user_marks::{persist_unplayed_override, resolve_mark_unit, write_favorite_mark};

pub(crate) fn default_library_cover(kind: MediaKind) -> Result<Vec<u8>, String> {
    const DEFAULT_MOVIE_COVER: &[u8] = include_bytes!("../assets/covers/movie.jpg");
    const DEFAULT_TV_COVER: &[u8] = include_bytes!("../assets/covers/tv.jpg");
    const DEFAULT_VIDEO_COVER: &[u8] = include_bytes!("../assets/covers/video.jpg");

    let bytes = match kind {
        MediaKind::Movie => DEFAULT_MOVIE_COVER,
        MediaKind::Tv => DEFAULT_TV_COVER,
        MediaKind::Video => DEFAULT_VIDEO_COVER,
    };
    Ok(bytes.to_vec())
}

fn library_cover_tag(cover_path: Option<&std::path::Path>, kind: MediaKind) -> String {
    if let Some(path) = cover_path {
        if let Ok(meta) = std::fs::metadata(path) {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            return format!("cover-{:x}-{:x}", meta.len(), mtime);
        }
    }
    let bytes = default_library_cover(kind).unwrap_or_default();
    format!("cover-{:x}", Sha256::digest(bytes))
}

fn catalog_episode_still_url(path: &str) -> Option<String> {
    if path.starts_with("https://image.tmdb.org/") || path.starts_with("http://image.tmdb.org/") {
        Some(path.to_string())
    } else if path.starts_with("/t/p/") {
        Some(format!("https://image.tmdb.org{path}"))
    } else if path.starts_with('/') {
        Some(format!("https://image.tmdb.org/t/p/w300{path}"))
    } else {
        None
    }
}

async fn resolve_catalog_episode_still(
    catalog: std::sync::Arc<dyn crate::catalog::Catalog>,
    tmdb_id: String,
    season: u32,
    episode: u32,
) -> Option<String> {
    let lookup_id = tmdb_id.clone();
    match tokio::task::spawn_blocking(move || catalog.episode_stills(&lookup_id, season, episode))
        .await
    {
        Ok(Ok(paths)) => paths
            .first()
            .and_then(|path| catalog_episode_still_url(path)),
        Ok(Err(error)) => {
            tracing::warn!(%error, tmdb_id, season, episode, "failed to resolve Jellyfin episode still from catalog");
            None
        }
        Err(error) => {
            tracing::warn!(%error, tmdb_id, season, episode, "Jellyfin episode still lookup task failed");
            None
        }
    }
}
pub struct ApiServerProvider {
    pub state: ApiState,
}

impl ApiServerProvider {
    pub fn new(state: ApiState) -> Self {
        Self { state }
    }
}

#[async_trait]
impl MediaServerProvider for ApiServerProvider {
    fn server_id(&self) -> String {
        self.state.jellyfin_server_id().to_string()
    }

    async fn authenticate_password(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<(String, ServerUser)>, AuthenticateError> {
        let authenticated = self
            .state
            .password_login
            .authenticate(
                self.state.store(),
                username.to_string(),
                password.to_string(),
            )
            .await;
        let (token, user) = match authenticated {
            Ok(Some(authenticated)) => authenticated,
            Ok(None) => return Ok(None),
            Err(crate::password_login::LoginError::Busy) => return Err(AuthenticateError::Busy),
            Err(error) => {
                tracing::error!(%error, "Jellyfin 密码登录失败");
                return Err(AuthenticateError::Internal(error.to_string()));
            }
        };
        Ok(Some((
            token,
            ServerUser {
                id: user.id,
                login: user.login,
                is_admin: user.role.as_str() == "admin",
            },
        )))
    }

    async fn user_id_by_token(&self, token: &str) -> Result<Option<UserId>, String> {
        let store = self.state.store.lock();
        store.user_id_by_token(token).map_err(|e| e.to_string())
    }

    /// 先 resolve 可见行，再交给共享 single-flight 字幕服务。
    async fn deliver_subtitle(
        &self,
        user_id: UserId,
        item_id: &str,
        index: u32,
        wants_vtt: bool,
    ) -> Result<Option<library::SubtitlePayload>, SubtitleDeliveryError> {
        let snapshot = self
            .resolve_single_item(user_id, item_id)
            .await
            .map_err(SubtitleDeliveryError::Failed)?;
        let Some(snapshot) = snapshot else {
            return Ok(None);
        };
        let request = crate::subtitle_delivery::SubtitleRequest {
            ledger_id: snapshot.file.file_id,
            source_path: snapshot.file.path.clone(),
            kind: snapshot.media.kind,
            tracks: snapshot.metadata.tracks,
            index,
            wants_vtt,
        };
        match self
            .state
            .subtitle_delivery
            .deliver(&self.state, request)
            .await
        {
            Ok(payload) => Ok(Some(payload)),
            Err(SubtitleServiceError::Delivery(
                DeliveryError::TrackNotFound | DeliveryError::FileMissing | DeliveryError::Io(_),
            )) => Ok(None),
            Err(SubtitleServiceError::Delivery(DeliveryError::Extraction(message))) => {
                Err(SubtitleDeliveryError::Failed(message))
            }
            Err(SubtitleServiceError::Busy) => Err(SubtitleDeliveryError::Busy),
            Err(SubtitleServiceError::Timeout) => Err(SubtitleDeliveryError::Timeout),
            Err(SubtitleServiceError::Cancelled) => Err(SubtitleDeliveryError::Cancelled),
            Err(SubtitleServiceError::Worker(message)) => {
                Err(SubtitleDeliveryError::Failed(message))
            }
        }
    }

    async fn bind_playback_credential(
        &self,
        user_id: UserId,
        device_id: &str,
        token: &str,
    ) -> Result<(), String> {
        self.state
            .store
            .lock()
            .bind_playback_credential(user_id, device_id, token)
            .map_err(|error| error.to_string())
    }

    async fn get_user(&self, user_id: UserId) -> Result<Option<ServerUser>, String> {
        let store = self.state.store.lock();
        let Some(user) = store.get_user(user_id).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let role = store.user_role(user.id).unwrap_or_else(|_| "member".into());
        Ok(Some(ServerUser {
            id: user.id,
            login: user.login,
            is_admin: role == "admin",
        }))
    }

    async fn list_visible_libraries(&self, user_id: UserId) -> Result<Vec<ServerLibrary>, String> {
        let state = self.state.clone();
        let budget = state.http_work.clone();
        budget
            .run(std::time::Duration::from_secs(30), move || {
                let store = state.store.lock();
                let ownership = store
                    .library_ownership()
                    .map_err(|error| error.to_string())?;
                let role = store
                    .user_role(user_id)
                    .map_err(|error| error.to_string())?;
                let libraries = store.list_libraries().map_err(|error| error.to_string())?;
                let mut list = Vec::new();
                for lib in libraries {
                    let visible = ownership.accepts_work(&lib.id)
                        && if role == "admin" {
                            lib.admin_visible
                        } else {
                            lib.access_mode == "everyone" || lib.member_ids.contains(&user_id)
                        };
                    if !visible {
                        continue;
                    }
                    let cover_path = crate::http::library::library_cover_path(&store, &lib);
                    let cover_tag = library_cover_tag(cover_path.as_deref(), lib.kind);
                    list.push(ServerLibrary {
                        id: lib.id,
                        name: lib.name,
                        kind: lib.kind,
                        exclude_from_home: lib.exclude_from_home,
                        root_paths: lib.root_paths,
                        cover_path: cover_path.map(|path| path.display().to_string()),
                        cover_tag,
                    });
                }
                Ok::<_, String>(list)
            })
            .await
            .map_err(|error| error.to_string())?
    }

    async fn list_visible_items(
        &self,
        user_id: UserId,
        parent_id: Option<&str>,
    ) -> Result<Vec<MediaItemSnapshot>, String> {
        items::list_visible_items(self, user_id, parent_id).await
    }
    async fn list_visible_items_in_library(
        &self,
        user_id: UserId,
        parent_id: Option<&str>,
        library_id: &str,
    ) -> Result<Vec<MediaItemSnapshot>, String> {
        items::list_visible_items_in_library(self, user_id, parent_id, library_id).await
    }

    /// 生产路径：轻候选 → 完整筛选/排序/计数 → 只为本页构建重元数据。
    async fn query_visible_items(
        &self,
        user_id: UserId,
        query: ServerItemQuery,
    ) -> Result<ServerItemPage, String> {
        selection::query_visible_items(self, user_id, query).await
    }

    /// 生产路径：一次候选读 + 每 Media 第一条未看 + 整体分页。
    async fn next_up_items(
        &self,
        user_id: UserId,
        series_id: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<ServerItemPage, String> {
        selection::next_up_items(self, user_id, series_id, offset, limit).await
    }

    /// 生产路径：续播候选在锁内选完，只为返回页构建重元数据。
    async fn resumable_items_page(
        &self,
        user_id: UserId,
        offset: usize,
        limit: usize,
    ) -> Result<ServerItemPage, String> {
        selection::resumable_items(self, user_id, offset, limit).await
    }
    async fn resolve_single_item(
        &self,
        user_id: UserId,
        id: &str,
    ) -> Result<Option<MediaItemSnapshot>, String> {
        match items::entry_snapshot(&self.state, user_id, id)? {
            Some(snapshot) => Ok(Some(self.enrich_single_item(snapshot).await)),
            None => Ok(None),
        }
    }

    async fn resolve_user_item_data(
        &self,
        user_id: UserId,
        id: &str,
    ) -> Result<Option<media_server::provider::UserItemData>, String> {
        let Some(item) = domain::TypedItemId::from_wire_string(id) else {
            return Ok(None);
        };
        if let domain::TypedItemId::Season { media, season } = item {
            return self
                .season_user_item_data(user_id, media, season)
                .await
                .map(Some);
        }
        Ok(self
            .resolve_single_item(user_id, id)
            .await?
            .map(|snapshot| snapshot.user_item_data()))
    }

    async fn enrich_visible_item(&self, item: MediaItemSnapshot) -> MediaItemSnapshot {
        self.enrich_single_item(item).await
    }

    async fn resolve_cover_bytes(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let store = self.state.store.lock();
        if let Ok(Some(lib)) = store.get_library(id) {
            if crate::http::library::library_visible(&store, &lib, user_id) {
                if let Some(cover) = crate::http::library::library_cover_path(&store, &lib) {
                    drop(store);
                    return std::fs::read(cover).map(Some).map_err(|e| e.to_string());
                }
                let kind = lib.kind;
                drop(store);
                return default_library_cover(kind).map(Some);
            }
        }
        Ok(None)
    }

    async fn resolve_poster_bytes(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let entry = {
            let store = self.state.store.lock();
            items::resolve_entry(&store, user_id, id)?
        };
        let Some(entry) = entry else {
            return Ok(None);
        };
        let path = if entry.is_series {
            crate::http::library::poster_path(&entry.file)
        } else {
            local_primary_image(&entry.file, entry.unit, &entry.media)
                .or_else(|| crate::http::library::poster_path(&entry.file))
        };
        match path {
            Some(path) => std::fs::read(path).map(Some).map_err(|e| e.to_string()),
            None => Ok(None),
        }
    }

    async fn resolve_primary_image_url(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<String>, String> {
        let (adopted_url, local_still, tmdb_episode) = {
            let store = self.state.store.lock();
            let entry = items::resolve_entry(&store, user_id, id)?;
            let Some(entry) = entry else {
                return Ok(None);
            };
            if entry.media.kind != MediaKind::Tv || entry.is_series {
                return Ok(None);
            }
            // 旧 `(season, episode)` 的 both-or-neither：整片没有分集坐标，端点直接放弃。
            let Some((season, episode)) =
                items::snapshot_unit(entry.unit).and_then(|unit| unit.episode_coordinates())
            else {
                return Ok(None);
            };
            // 与页内同一个已采纳字段入口：图片端点不再自己读现场 NFO。
            let adopted_still = item_metadata::page_fields(
                &store,
                &entry.file,
                entry.unit,
                &entry.media,
                entry.is_series,
            )
            .ok()
            .and_then(|fields| fields.still_url.value().cloned());
            let adopted_url = primary_image_url(entry.unit, &entry.media, adopted_still.as_deref());
            let local_still = crate::episode_still::existing(entry.file.path.as_path()).is_some();
            let tmdb_episode = entry
                .media
                .tmdb_id
                .filter(|_| !local_still && adopted_url.is_none())
                .map(|tmdb_id| (tmdb_id, season, episode));
            (adopted_url, local_still, tmdb_episode)
        };
        if local_still {
            return Ok(None);
        }
        if adopted_url.is_some() {
            return Ok(adopted_url);
        }
        if let Some((tmdb_id, season, episode)) = tmdb_episode {
            return Ok(resolve_catalog_episode_still(
                self.state.catalog.clone(),
                tmdb_id,
                season,
                episode,
            )
            .await);
        }
        Ok(None)
    }

    async fn resolve_person_image_url(
        &self,
        user_id: Option<domain::UserId>,
        name: &str,
    ) -> Result<Option<String>, String> {
        person_images::resolve_person_image_url(self, user_id, name).await
    }

    async fn resolve_person(
        &self,
        user_id: UserId,
        name: &str,
    ) -> Result<Option<MediaPerson>, String> {
        person_images::resolve_person(self, user_id, name).await
    }

    async fn resolve_backdrop_bytes(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let entry = {
            let store = self.state.store.lock();
            items::resolve_entry(&store, user_id, id)?
        };
        let Some(entry) = entry else {
            return Ok(None);
        };
        let Some(path) = backdrop_path(&entry.file, entry.is_series) else {
            return Ok(None);
        };
        std::fs::read(path)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    /// Stream routing: the public item id names the logical target, and the
    /// stream is the published, present source selected for it. A pending,
    /// missing or unknown file is never offered.
    async fn resolve_stream_source(
        &self,
        user_id: UserId,
        id: &str,
    ) -> Result<Option<(PathBuf, bool)>, String> {
        self.resolve_stream_source_bound(user_id, id, None, None, None)
            .await
    }

    async fn resolve_stream_source_bound(
        &self,
        user_id: UserId,
        id: &str,
        media_source_id: Option<&str>,
        play_session_id: Option<&str>,
        device_id: Option<&str>,
    ) -> Result<Option<(PathBuf, bool)>, String> {
        // Must not accept bare file UUID as a logical ID
        if id.len() == 36
            && id.chars().filter(|c| *c == '-').count() == 4
            && uuid::Uuid::parse_str(id).is_ok()
        {
            return Ok(None);
        }
        let entry = {
            let store = self.state.store.lock();
            items::resolve_entry(&store, Some(user_id), id)?
        };
        let Some(entry) = entry else {
            return Ok(None);
        };

        let file = if let Some(msid) = media_source_id {
            // Only the versioned `f1_<file>_<revision>` shape advertised by
            // PlaybackInfo is accepted; a bare file UUID is not a MediaSourceId.
            let Some(physical) = domain::PhysicalSourceId::from_wire_string(msid) else {
                return Ok(None);
            };
            let store = self.state.store.lock();
            let Some(file) = store
                .visible_file(user_id, physical.file_id)
                .map_err(|e| e.to_string())?
            else {
                return Ok(None);
            };
            let covers = store
                .file_covers_unit(
                    physical.file_id,
                    entry.media.id,
                    domain::UnitKey::new(entry.unit.0, entry.unit.1),
                )
                .map_err(|e| e.to_string())?;
            if !covers
                || file.media_id != entry.media.id
                || file.version.revision != physical.revision
            {
                return Ok(None);
            }
            file
        } else {
            entry.file.clone()
        };
        if let Some(session_id) = play_session_id
            && !self.stream_matches_binding(user_id, device_id, session_id, &entry, &file)?
        {
            tracing::warn!(%user_id, item_id = %id, "Jellyfin 取流与签发的播放会话绑定不符，拒绝");
            return Ok(None);
        }

        let path = file.path.clone();
        let is_strm = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"));
        Ok(Some((path, is_strm)))
    }

    async fn is_device_revoked(&self, user_id: UserId, device_id: &str) -> Result<bool, String> {
        let store = self.state.store.lock();
        store
            .is_device_revoked(user_id, device_id)
            .map_err(|e| e.to_string())
    }

    async fn set_user_marks(
        &self,
        user_id: UserId,
        id: &str,
        played: Option<bool>,
        favorite: Option<bool>,
    ) -> Result<bool, String> {
        let store = self.state.store.lock();
        let Some((media_id, season, episode)) = resolve_mark_unit(&store, user_id, id)? else {
            return Ok(false);
        };
        // 播放域时间走注入时钟；`playback_units.updated_at` 是**秒**。
        let now = self.state.playback_clock.now_ms() / 1000;
        if played.is_none() {
            let Some(favorite) = favorite else {
                return Ok(true);
            };
            write_favorite_mark(&store, user_id, media_id, season, episode, favorite, now)?;
            if !favorite
                && season == crate::store::UNIT_WHOLE
                && episode == crate::store::UNIT_WHOLE
            {
                let _ = store.set_unit_marks(
                    user_id,
                    media_id,
                    season,
                    episode,
                    None,
                    Some(false),
                    now,
                );
            }
            return Ok(true);
        }
        store
            .set_unit_marks(user_id, media_id, season, episode, played, favorite, now)
            .map_err(|error| {
                tracing::error!(%error, %user_id, %media_id, season, episode, "failed to update Jellyfin user marks");
                error.to_string()
            })?;
        if let Some(false) = played {
            persist_unplayed_override(&store, user_id, media_id, season, episode, favorite, now)?;
        }
        Ok(true)
    }

    async fn issue_play_session(
        &self,
        user_id: UserId,
        device_id: &str,
        id: &str,
    ) -> Result<String, String> {
        let target = playback::resolve_target(self, user_id, id)?
            .ok_or_else(|| format!("Playback item {id} missing or not visible"))?;
        use std::str::FromStr;
        let ledger_id = domain::LedgerId::from_str(&target.ledger_id).map_err(|e| e.to_string())?;
        // 播放域时间走注入时钟；`issue_play_session` 的 `issued_at` 是**秒**。
        let now = self.state.playback_clock.now_ms() / 1000;
        let store = self.state.store.lock();
        store
            .issue_play_session(
                user_id,
                device_id,
                ledger_id,
                target.media.id,
                domain::UnitKey::new(target.season, target.episode),
                &target.file.version,
                now,
            )
            .map_err(|e| e.to_string())
    }

    async fn report_playback_event(
        &self,
        user_id: UserId,
        id: &str,
        position_ms: i64,
        paused: bool,
        client: PlaybackClientInfo,
        event: PlaybackEvent,
        identity: media_server::provider::PlaybackEventIdentity,
    ) -> Result<(), String> {
        playback::report_event(
            self,
            user_id,
            id,
            position_ms,
            paused,
            client,
            event,
            identity,
        )
    }
}

impl ApiServerProvider {
    /// Season-level marks rolled up over the season's visible episodes.
    async fn season_user_item_data(
        &self,
        user_id: UserId,
        media: domain::MediaId,
        season: u32,
    ) -> Result<media_server::provider::UserItemData, String> {
        let series_id = domain::TypedItemId::Work(media).to_wire_string();
        let episodes: Vec<MediaItemSnapshot> = self
            .list_visible_items(user_id, Some(&series_id))
            .await?
            .into_iter()
            .filter(|episode| episode.unit.map(|unit| unit.season) == Some(season as i32))
            .collect();
        let total = episodes.len();
        let played = episodes.iter().filter(|episode| episode.played).count();
        let favorite = {
            let store = self.state.store.lock();
            store
                .unit_state(user_id, media, season as i32, crate::store::UNIT_WHOLE)
                .map_err(|error| error.to_string())?
                .is_some_and(|state| state.favorite)
        };
        let unplayed_item_count = total.saturating_sub(played);
        let item_id = domain::TypedItemId::Season { media, season }.to_wire_string();
        Ok(media_server::provider::UserItemData {
            key: item_id.clone(),
            item_id,
            playback_position_ticks: 0,
            play_count: 0,
            played: total == 0 || unplayed_item_count == 0,
            is_favorite: favorite,
            unplayed_item_count: Some(unplayed_item_count),
            played_percentage: (total > 0).then(|| played as f64 / total as f64 * 100.0),
        })
    }
}

/// Logical identity of one stored watch unit.
fn resume_item_id(media_id: domain::MediaId, season: i32, episode: i32) -> domain::TypedItemId {
    if season < 0 || episode < 0 {
        return domain::TypedItemId::Work(media_id);
    }
    domain::TypedItemId::Unit {
        media: media_id,
        key: domain::UnitKey::new(season, episode),
    }
}

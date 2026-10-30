use async_trait::async_trait;
use domain::{MediaKind, UserId};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use media_server::provider::{
    AuthenticateError, MediaItemSnapshot, MediaPerson, MediaServerProvider, PlaybackClientInfo,
    PlaybackEvent, ServerItemPage, ServerItemQuery, ServerLibrary, ServerUser,
    SubtitleDeliveryError, UserMarksError,
};

use crate::subtitle_delivery::SubtitleServiceError;
use library::DeliveryError;

use crate::management::ApiState;
mod query_lookups;
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
mod sync_operations;
use item_metadata::{
    backdrop_path, item_metadata, local_primary_image, merge_catalog_metadata, primary_image_url,
};

mod enrichment;
mod entities;
pub(crate) mod items;
mod selection;
mod user_marks;
use user_marks::resolve_mark_unit;

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
    if let Some(path) = cover_path
        && let Ok(meta) = std::fs::metadata(path)
    {
        let mtime = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        return format!("cover-{:x}-{:x}", meta.len(), mtime);
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

    fn record_query_phase(&self, phase: &'static str, duration: std::time::Duration) {
        self.state.record_query_phase(phase, duration);
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
                self.state.clone(),
                username.to_string(),
                password.to_string(),
            )
            .await;
        let (token, user) = match authenticated {
            Ok(Some(authenticated)) => authenticated,
            Ok(None) => return Ok(None),
            Err(crate::password_login::LoginError::Busy) => return Err(AuthenticateError::Busy),
            Err(crate::password_login::LoginError::Admission(response)) => {
                return Err(if response.status() == axum::http::StatusCode::CONFLICT {
                    AuthenticateError::ReadOnly
                } else {
                    AuthenticateError::Busy
                });
            }
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

    async fn user_id_by_token(
        &self,
        token: &str,
        deadline: Option<std::time::Instant>,
    ) -> Result<Option<UserId>, media_server::provider::QueryError> {
        // token→用户走共享只读 Store 池，
        // 不再排在进程级 `Store` 锁后面——否则扫描期间 Jellyfin 协议面（Infuse/
        // Emby 客户端）与自有 API 一样每个请求都 503。阶段名仍是 `auth`：
        // `library_read_metrics` 按 `timeout_stage == "auth"` 观察。
        let deadline = deadline.unwrap_or_else(|| {
            std::time::Instant::now() + crate::http::store_wait::STORE_READ_WAIT
        });
        let token = token.to_owned();
        crate::library_read_work::run_app_read_stage_named(
            &self.state,
            deadline,
            "auth",
            move |reader| reader.user_id_by_token(&token),
        )
        .await
        .map_err(query_stage_error)
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
        deadline: Option<std::time::Instant>,
    ) -> Result<(), media_server::provider::QueryError> {
        if !self.state.instance_writable() {
            tracing::warn!(%user_id, "Playback credential observation skipped in read-only instance");
            return Ok(());
        }
        let deadline = deadline.unwrap_or_else(|| {
            std::time::Instant::now() + crate::http::store_wait::STORE_WRITE_WAIT
        });

        let read_device = device_id.to_owned();
        let read_token = token.to_owned();
        let needed = crate::library_read_work::run_store_query_stage_named(
            &self.state,
            deadline,
            "credential_binding",
            move |store| {
                store
                    .playback_credential_needs_binding(user_id, &read_device, &read_token)
                    .map_err(Into::into)
            },
        )
        .await
        .map_err(query_stage_error)?;
        if !needed {
            return Ok(());
        }
        let device_id = device_id.to_owned();
        let token = token.to_owned();
        crate::library_read_work::run_store_write_stage_named(
            &self.state,
            deadline,
            "credential_binding",
            move |store| {
                store
                    .bind_playback_credential(user_id, &device_id, &token)
                    .map_err(Into::into)
            },
        )
        .await
        .map_err(query_stage_error)
    }

    async fn get_user(&self, user_id: UserId) -> Result<Option<ServerUser>, String> {
        self.run_sync(move |provider| provider.get_user_sync(user_id))
            .await?
    }

    async fn get_user_for_query(
        &self,
        principal: media_server::provider::QueryPrincipal,
    ) -> Result<Option<ServerUser>, media_server::provider::QueryError> {
        query_lookups::get_user_for_query(&self.state, principal).await
    }

    async fn list_visible_libraries_for_query(
        &self,
        principal: media_server::provider::QueryPrincipal,
    ) -> Result<Vec<ServerLibrary>, media_server::provider::QueryError> {
        query_lookups::list_visible_libraries_for_query(&self.state, principal).await
    }

    async fn list_visible_libraries(&self, user_id: UserId) -> Result<Vec<ServerLibrary>, String> {
        let state = self.state.clone();
        let budget = state.http_work.clone();
        budget
            .run(std::time::Duration::from_secs(30), move || {
                let libraries = sync_operations::read_store(&state, |store| {
                    let ownership = store
                        .library_ownership()
                        .map_err(|error| error.to_string())?;
                    let role = store
                        .user_role(user_id)
                        .map_err(|error| error.to_string())?;
                    let libraries = store.list_libraries().map_err(|error| error.to_string())?;
                    let libraries = libraries
                        .into_iter()
                        .filter(|lib| {
                            ownership.accepts_work(&lib.id)
                                && if role == "admin" {
                                    lib.admin_visible
                                } else {
                                    lib.access_mode == "everyone"
                                        || lib.member_ids.contains(&user_id)
                                }
                        })
                        .collect::<Vec<_>>();
                    Ok(libraries)
                })?;
                let mut list = Vec::new();
                for lib in libraries {
                    let cover_path = lib
                        .cover_path
                        .as_deref()
                        .filter(|path| !path.is_empty() && *path != "none")
                        .map(PathBuf::from)
                        .filter(|path| path.is_file());
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

    async fn query_entities(
        &self,
        user: UserId,
        kind: media_server::provider::EntityKind,
        parent: Option<&str>,
        search: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<media_server::provider::EntityPage, String> {
        entities::entities(self, user, kind, parent, search, offset, limit).await
    }
    async fn query_item_counts(
        &self,
        user: UserId,
    ) -> Result<media_server::provider::ItemCounts, String> {
        entities::counts(self, user).await
    }
    async fn query_seasons(
        &self,
        user: UserId,
        media: domain::MediaId,
    ) -> Result<Vec<media_server::provider::Season>, String> {
        entities::seasons(self, user, media).await
    }
    async fn search_hints_page(
        &self,
        user: UserId,
        term: &str,
        offset: usize,
        limit: usize,
    ) -> Result<ServerItemPage, String> {
        entities::hints(self, user, term, offset, limit).await
    }
    async fn resolve_entity_image(
        &self,
        user: Option<UserId>,
        kind: media_server::provider::EntityKind,
        query: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        entities::image(self, user, kind, query).await
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
        principal: media_server::provider::QueryPrincipal,
        query: ServerItemQuery,
    ) -> Result<ServerItemPage, media_server::provider::QueryError> {
        selection::query_visible_items(self, principal, query).await
    }

    async fn query_latest_items(
        &self,
        principal: media_server::provider::QueryPrincipal,
        query: ServerItemQuery,
    ) -> Result<Vec<MediaItemSnapshot>, media_server::provider::QueryError> {
        selection::query_latest_items(self, principal, query).await
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
        let id = id.to_owned();
        self.run_sync(move |provider| provider.resolve_cover_bytes_sync(user_id, &id))
            .await?
    }

    async fn resolve_poster_bytes(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let id = id.to_owned();
        self.run_sync(move |provider| provider.resolve_poster_bytes_sync(user_id, &id))
            .await?
    }

    async fn resolve_primary_image_url(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<String>, String> {
        let id = id.to_owned();
        let facts = self
            .run_sync(move |provider| -> Result<_, String> {
                let facts = provider.bounded_reader(|store| {
                    let Some(entry) = items::resolve_entry(store, user_id, &id)? else {
                        return Ok(None);
                    };
                    if entry.media.kind != MediaKind::Tv || entry.is_series {
                        return Ok(None);
                    }
                    // 旧 `(season, episode)` 的 both-or-neither：整片没有分集坐标，端点直接放弃。
                    let Some((season, episode)) = items::snapshot_unit(entry.unit)
                        .and_then(|unit| unit.episode_coordinates())
                    else {
                        return Ok(None);
                    };
                    // 与页内同一个已采纳字段入口：图片端点不再自己读现场 NFO。
                    let adopted_still = item_metadata::page_fields(
                        store,
                        &entry.file,
                        entry.unit,
                        &entry.media,
                        entry.is_series,
                    )
                    .ok()
                    .and_then(|fields| fields.still_url.value().cloned());
                    let adopted_url =
                        primary_image_url(entry.unit, &entry.media, adopted_still.as_deref());
                    Ok(Some((entry, adopted_url, season, episode)))
                })?;
                let Some((entry, adopted_url, season, episode)) = facts else {
                    return Ok(None);
                };
                let local_still =
                    crate::episode_still::existing(entry.file.path.as_path()).is_some();
                let tmdb_episode = entry
                    .media
                    .tmdb_id
                    .filter(|_| !local_still && adopted_url.is_none())
                    .map(|tmdb_id| (tmdb_id, season, episode));
                Ok(Some((adopted_url, local_still, tmdb_episode)))
            })
            .await??;
        let Some((adopted_url, local_still, tmdb_episode)) = facts else {
            return Ok(None);
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

    async fn deliver_person_image(
        &self,
        user: Option<UserId>,
        query: &str,
        headers: axum::http::HeaderMap,
    ) -> Result<Option<axum::response::Response>, String> {
        person_images::deliver_person_image(self, user, query, headers).await
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
        let id = id.to_owned();
        self.run_sync(move |provider| provider.resolve_backdrop_bytes_sync(user_id, &id))
            .await?
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
        let id = id.to_owned();
        let media_source_id = media_source_id.map(str::to_owned);
        let play_session_id = play_session_id.map(str::to_owned);
        let device_id = device_id.map(str::to_owned);
        self.run_sync(move |provider| {
            provider.resolve_stream_source_bound_sync(
                user_id,
                &id,
                media_source_id.as_deref(),
                play_session_id.as_deref(),
                device_id.as_deref(),
            )
        })
        .await?
    }

    async fn is_device_revoked(&self, user_id: UserId, device_id: &str) -> Result<bool, String> {
        let device_id = device_id.to_owned();
        self.run_sync(move |provider| provider.is_device_revoked_sync(user_id, &device_id))
            .await?
    }

    async fn set_user_marks(
        &self,
        user_id: UserId,
        id: &str,
        played: Option<bool>,
        favorite: Option<bool>,
    ) -> Result<bool, UserMarksError> {
        let id = id.to_owned();
        let provider = Self::new(self.state.clone());
        self.state
            .http_work
            .run_to_completion(move || provider.set_user_marks_sync(user_id, &id, played, favorite))
            .await
            .map_err(|error| match error {
                crate::blocking_budget::BlockingError::Busy => UserMarksError::Unavailable,
                error => UserMarksError::Internal(error.to_string()),
            })?
    }

    async fn issue_play_session(
        &self,
        user_id: UserId,
        device_id: &str,
        id: &str,
    ) -> Result<String, String> {
        let id = id.to_owned();
        let device_id = device_id.to_owned();
        self.run_sync(move |provider| provider.issue_play_session_sync(user_id, &device_id, &id))
            .await?
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
        let id = id.to_owned();
        self.run_sync(move |provider| {
            provider.report_playback_event_sync(
                user_id,
                &id,
                position_ms,
                paused,
                client,
                event,
                identity,
            )
        })
        .await?
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
        let summary = entities::seasons(self, user_id, media)
            .await?
            .into_iter()
            .find(|row| row.season == season);
        let (total, played) = summary
            .map(|row| (row.total as usize, row.played as usize))
            .unwrap_or_default();
        let favorite = self
            .run_sync(move |provider| {
                provider.bounded_reader(|store| {
                    store
                        .unit_state(user_id, media, season as i32, crate::store::UNIT_WHOLE)
                        .map(|state| state.is_some_and(|state| state.favorite))
                        .map_err(|error| error.to_string())
                })
            })
            .await??;
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

fn query_stage_error(error: store::LibraryReadError) -> media_server::provider::QueryError {
    if crate::library_read_work::error_is_unavailable(&error) {
        media_server::provider::QueryError::Unavailable
    } else if matches!(error, store::LibraryReadError::Unauthorized) {
        media_server::provider::QueryError::Unauthorized
    } else {
        tracing::error!(%error, "Jellyfin Library query stage failed");
        media_server::provider::QueryError::Internal(error.to_string())
    }
}
#[cfg(test)]
mod tests;

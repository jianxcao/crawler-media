//! File-local chapters and immutable scene frames for the detail strip.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::http::library::require_visible_library;
use crate::http::{err, ok, ok_list};
use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;
use std::str::FromStr;

mod images;
pub(crate) use images::chapter_image;
use images::chapter_items_off_executor;
mod frame_status;
pub(crate) use frame_status::frame_status;
mod refresh;
pub(crate) use refresh::{probe_status, refresh_item_chapters};

/// Transfer/scan publish physical chapters and their durable frame obligation.
/// Cached metadata avoids another read; failures are logged without failing Transfer.
pub(crate) fn auto_generate_chapters(state: &ApiState, path: &std::path::Path) {
    if !chapter_images_enabled(&state.store, path) {
        return;
    }
    let row = match state.store.file_coverage_by_path(&path.to_string_lossy()) {
        Ok(Some(row)) => row,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%error, path=%path.display(), "chapter file lookup failed");
            return;
        }
    };
    let Some(unit) = row.units.first().copied() else {
        return;
    };
    let lease = match state.acquire_path_work(path, row.file.kind) {
        Ok(lease) => lease,
        Err(error) => {
            tracing::debug!(%error, "chapter generation rejected");
            return;
        }
    };
    let control = lease
        .as_ref()
        .map(|lease| lease.control().clone())
        .unwrap_or_default();
    let cached = match state
        .store
        .get_cached_chapters(&row.file.file_id.to_string())
    {
        Ok(cached) => cached,
        Err(error) => {
            tracing::warn!(%error, "chapter cache lookup failed");
            return;
        }
    };
    let chapters = match cached {
        Some(chapters) => chapters,
        None => match marker::probe_chapters_controlled(path, control.signal()) {
            Some(chapters) => marker::annotate_chapters(&chapters),
            None => return,
        },
    };
    if control.is_cancelled() {
        return;
    }
    match crate::fingerprint_job::source::observe_source(&row) {
        Ok(()) => {}
        Err(error) => {
            tracing::warn!(%error, file=%row.file.file_id, "chapter source changed before publication");
            return;
        }
    };
    let observed = store::SourceObservation {
        file_id: row.file.file_id,
        expected: row.file.version.clone(),
        presence: domain::FilePresence::Present,
    };
    let result = control.publish(|| {
        super::store_wait::with_foreground_store(state, |store| {
            store.library_write(|db| {
                db.publish_file_chapters_checked(&row, unit, &[observed], &[], Some(&chapters))
            })
        })
        .map_err(|response| format!("chapter publication unavailable: {}", response.status()))?
        .map_err(|error| error.to_string())
    });
    match result {
        Ok(()) => state.chapter_frames.notify_pending(),
        Err(error) => tracing::warn!(%error,file=%row.file.file_id,"chapter publication rejected"),
    }
}

/// 文件持久化归属库（`library_id`）的「生成章节」开关。未入账文件默认开启，
/// 与入库自动抓帧的历史行为一致。
pub(crate) fn chapter_images_enabled(store: &crate::Store, path: &std::path::Path) -> bool {
    owning_library(store, path).is_none_or(|lib| lib.extract_chapter_images)
}

/// The library persisted as owning the file at `path` (its row's `library_id`).
fn owning_library(store: &crate::Store, path: &std::path::Path) -> Option<store::Library> {
    let coverage = store
        .file_coverage_by_path(&path.to_string_lossy())
        .ok()
        .flatten()?;
    store
        .get_library(&coverage.file.library_id.to_string())
        .ok()
        .flatten()
}

#[derive(Clone, Copy, Deserialize)]
pub(crate) struct ChapterQuery {
    file_id: domain::LedgerId,
    season: Option<u32>,
    episode: Option<u32>,
}

fn selected_target(
    rows: &[domain::FileCoverage],
    query: ChapterQuery,
    kind: domain::MediaKind,
) -> Option<(domain::FileCoverage, domain::UnitKey)> {
    let unit = match (kind, query.season, query.episode) {
        (domain::MediaKind::Tv, Some(season), Some(episode)) => {
            domain::UnitKey::new(i32::try_from(season).ok()?, i32::try_from(episode).ok()?)
        }
        (domain::MediaKind::Tv, _, _) => return None,
        (_, None, None) => domain::UnitKey::WHOLE,
        _ => return None,
    };
    rows.iter()
        .find(|row| {
            row.file.file_id == query.file_id
                && row.file.presence == domain::FilePresence::Present
                && row.file.publication == domain::FilePublication::Published
                && row.units.contains(&unit)
        })
        .map(|row| (row.clone(), unit))
}

fn covers_season(row: &domain::FileCoverage, season: u32) -> bool {
    row.units.iter().any(|unit| {
        unit.episode_coordinates()
            .is_some_and(|coordinates| coordinates.0 == season)
    })
}

fn theintrodb_client(store: &crate::Store) -> Option<crate::theintrodb::TheIntroDbClient> {
    let config = store.get_scrape_config().ok()?;
    let effective = &config.effective;
    (effective.theintrodb_enabled && effective.theintrodb_api_key.is_some())
        .then(|| crate::theintrodb::TheIntroDbClient::new(effective.theintrodb_api_key.clone()))
}

struct ChapterTarget {
    row: domain::FileCoverage,
    unit: domain::UnitKey,
    media: domain::Media,
    rows: Vec<domain::FileCoverage>,
    images_enabled: bool,
    theintrodb: Option<crate::theintrodb::TheIntroDbClient>,
}

async fn capture_target(
    state: &ApiState,
    id: String,
    media_id: domain::MediaId,
    user: Option<domain::UserId>,
    query: ChapterQuery,
) -> Result<ChapterTarget, Response> {
    super::store_wait::store_read(state, &[store::Db::Library, store::Db::App], move |store| {
        let library = require_visible_library(store, &id, user)?;
        let media = store
            .get_media(media_id)
            .map_err(chapter_store_error)?
            .ok_or_else(|| err(StatusCode::NOT_FOUND, "library.item_missing", "条目不存在"))?;
        let rows = store
            .file_coverage_in_library(
                library
                    .id
                    .parse()
                    .map_err(|error: uuid::Error| chapter_store_error(error.into()))?,
                Some(media_id),
                store::FileReadScope::AllRecorded,
            )
            .map_err(chapter_store_error)?;
        let (row, unit) = selected_target(&rows, query, media.kind).ok_or_else(|| {
            err(
                StatusCode::NOT_FOUND,
                "library.item_missing",
                "条目没有在位文件",
            )
        })?;
        let theintrodb = theintrodb_client(store);
        Ok(ChapterTarget {
            row,
            unit,
            media,
            rows,
            images_enabled: library.extract_chapter_images,
            theintrodb,
        })
    })
    .await
    .map_err(|error| error.response())?
}

fn chapter_store_error(error: store::StoreError) -> Response {
    tracing::error!(%error, "读取章节事实失败");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}

async fn bounded_library_lease(
    state: &ApiState,
    id: String,
) -> Result<std::sync::Arc<crate::library_lifecycle::LibraryWorkLease>, Response> {
    let worker = state.clone();
    match state
        .http_work
        .run_to_completion(move || worker.library_work().acquire_bounded(&worker.store, &id))
        .await
    {
        Ok(Ok(lease)) => Ok(std::sync::Arc::new(lease)),
        Ok(Err(
            crate::library_lifecycle::BoundedLeaseError::StoreBusy
            | crate::library_lifecycle::BoundedLeaseError::RegistryBusy,
        )) => Err(super::store_wait::busy_response()),
        Ok(Err(error)) => Err(err(
            StatusCode::CONFLICT,
            "library.frozen",
            &error.to_string(),
        )),
        Err(error) => Err(super::library::browse_work_error(error)),
    }
}

/// GET /libraries/{id}/items/{item_id}/chapters — probe chapters, with
/// `image_url` for the ones that already have a generated scene frame.
pub(crate) async fn chapters(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<domain::UserId>>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<ChapterQuery>,
) -> Response {
    let media_id = match domain::MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let ChapterTarget {
        row,
        unit,
        media,
        theintrodb: theintrodb_client,
        ..
    } = match capture_target(&state, id, media_id, user_id, query).await {
        Ok(target) => target,
        Err(response) => return response,
    };
    let resolved = crate::marker_resolver::resolve_item_chapters(
        &state,
        &row,
        unit,
        &media,
        theintrodb_client,
    )
    .await;

    if resolved.is_empty() {
        return ok_list(Vec::new()).into_response();
    }
    match chapter_items_off_executor(&state, row, resolved).await {
        Ok(items) => ok_list(items).into_response(),
        Err(response) => response,
    }
}

/// POST /libraries/{id}/items/{item_id}/chapters/generate — extract one scene
/// frame per chapter beside the file.
#[derive(Deserialize)]
pub(crate) struct GenerateChapterFrames {
    retry_failed: bool,
}

pub(crate) async fn generate_chapters(
    State(state): State<ApiState>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<ChapterQuery>,
    axum::Json(body): axum::Json<GenerateChapterFrames>,
) -> Response {
    if let Err(response) = state
        .require_writable()
        .and_then(|()| state.require_write_admitted())
    {
        return response;
    }
    let lease = match bounded_library_lease(&state, id.clone()).await {
        Ok(lease) => lease,
        Err(response) => return response,
    };
    let media_id = match domain::MediaId::from_str(&item_id) {
        Ok(id) => id,
        Err(_) => return err(StatusCode::BAD_REQUEST, "library.invalid", "条目 id 无效"),
    };
    let ChapterTarget {
        row,
        unit,
        media,
        images_enabled,
        theintrodb: theintrodb_client,
        ..
    } = match capture_target(&state, id, media_id, None, query).await {
        Ok(target) => target,
        Err(response) => return response,
    };
    if !images_enabled {
        return err(
            StatusCode::CONFLICT,
            "chapters.disabled",
            "该库已关闭场景图生成",
        );
    }
    if row.units.len() != 1 {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "chapters.unit_time_mapping_unsupported",
            "多单元文件缺少分集时间映射",
        );
    }
    let path = row.file.path.clone();
    let worker = state.clone();
    let probe_path = path.clone();
    let probe_lease = lease.clone();
    let probed = state
        .fs_work
        .reserve(std::time::Duration::from_secs(5))
        .await;
    let reservation = match probed {
        Ok(reservation) => reservation,
        Err(_) => return super::store_wait::busy_response(),
    };
    if let Err(error) = reservation
        .run_to_completion(move || {
            let _lease = probe_lease;
            auto_generate_chapters(&worker, &probe_path);
        })
        .await
    {
        return super::library::browse_work_error(error);
    }
    let _ = crate::marker_resolver::resolve_item_chapters(
        &state,
        &row,
        unit,
        &media,
        theintrodb_client,
    )
    .await;
    let file_id = row.file.file_id;
    let targets = match super::store_wait::store_read(&state, &[store::Db::Library], move |store| {
        store.current_chapter_frame_targets(file_id)
    })
    .await
    {
        Ok(Ok(targets)) => targets.unwrap_or_default(),
        Ok(Err(error)) => return chapter_store_error(error),
        Err(error) => return error.response(),
    };

    if targets.is_empty() {
        return ok(json!({ "task": Value::Null, "empty": true })).into_response();
    }

    let worker = state.clone();
    let queued = state
        .http_work
        .run_to_completion(move || {
            let _lease = lease;
            super::store_wait::with_foreground_store(&worker, |_| {
                worker
                    .chapter_frames
                    .enqueue_path(&path, targets, body.retry_failed)
                    .map_err(|error| {
                        tracing::warn!(%error, path=%path.display(), "章节帧任务入队失败");
                        err(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "chapters.enqueue_failed",
                            "章节帧任务入队失败，请稍后重试",
                        )
                    })
            })?
        })
        .await;
    match queued {
        Ok(Ok(_)) => {}
        Ok(Err(response)) => return response,
        Err(error) => return super::library::browse_work_error(error),
    }
    match frame_status::load_current_status(&state, row).await {
        Ok((task, empty)) => ok(json!({ "task": task, "empty": empty })).into_response(),
        Err(response) => response,
    }
}

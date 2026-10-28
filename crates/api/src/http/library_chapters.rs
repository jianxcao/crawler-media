//! Video chapters: container chapter markers with scene frames beside the
//! file (`chapter_N.jpg`), served for the detail strip and the player.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::http::library::{preferred_coverage, require_visible_library};
use crate::http::{err, ok, ok_list};
use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;
use std::str::FromStr;

mod refresh;
pub(crate) use refresh::{probe_status, refresh_item_chapters};

/// 自动生成：文件有内嵌章节且尚无章节帧时，逐章抽一帧（跳过已有）。
/// 供整库刷新与入库链路调用；失败静默（不影响主流程）。
pub(crate) fn auto_generate_chapters(state: &ApiState, path: &std::path::Path) {
    let enabled = {
        let store = state.store.lock();
        chapter_images_enabled(&store, path)
    };
    if !enabled {
        tracing::debug!(path = %path.display(), "媒体库已关闭章节场景图提取，跳过自动抓帧");
        return;
    }
    let kind = owning_library(&state.store.lock(), path).map(|owner| owner.kind);
    let lease = match kind
        .map(|kind| state.acquire_path_work(path, kind))
        .transpose()
    {
        Ok(lease) => lease.flatten(),
        Err(error) => {
            tracing::debug!(%error, "chapter generation rejected");
            return;
        }
    };
    let control = lease
        .as_ref()
        .map(|lease| lease.control().clone())
        .unwrap_or_default();
    if let Some(targets) = probe_chapter_targets(path, &control)
        && let Err(error) = state.chapter_frames.enqueue_path(path, targets)
    {
        tracing::debug!(%error, path = %path.display(), "章节帧任务入队失败");
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

pub(crate) fn trigger_scene_frames_for_chapter_updates(
    updates: &[(String, Vec<library::ChapterMarker>)],
    service: &std::sync::Arc<crate::chapter_frame_service::ChapterFrameService>,
) {
    for (ledger_id, chapters) in updates {
        if chapters.is_empty() {
            continue;
        }
        let targets: Vec<(i64, i64)> = chapters.iter().map(|c| (c.start_ms, c.end_ms)).collect();
        if targets.is_empty() {
            continue;
        }
        match service.enqueue_ledger(ledger_id, targets) {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                %ledger_id,
                "章节帧任务已存在或媒体库已关闭，跳过"
            ),
            Err(error) => tracing::debug!(%error, %ledger_id, "章节帧任务入队失败"),
        }
    }
}

fn probe_chapter_targets(
    path: &std::path::Path,
    control: &library::WorkControl,
) -> Option<Vec<(i64, i64)>> {
    let mut command = std::process::Command::new("ffprobe");
    command.args(["-v", "error", "-show_chapters", "-of", "json"]);
    library::ProbeTarget::from_path(path).apply_input(&mut command);
    let output = control
        .output(&mut command)
        .map_err(|error| {
            tracing::debug!(%error, path = %path.display(), "chapter probe stopped");
        })
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    Some(
        value["chapters"]
            .as_array()?
            .iter()
            .filter_map(|chapter| {
                let start = chapter["start_time"].as_str()?.parse::<f64>().ok()?;
                let end = chapter["end_time"].as_str()?.parse::<f64>().ok()?;
                Some(((start * 1000.0) as i64, (end * 1000.0) as i64))
            })
            .collect(),
    )
}

fn chapter_image_path(video: &std::path::Path, index: usize) -> std::path::PathBuf {
    crate::chapter_frame_service::chapter_frame_path(video, index)
}

#[derive(Clone, Copy, Default, Deserialize)]
pub(crate) struct ChapterQuery {
    season: Option<u32>,
    episode: Option<u32>,
}

fn selected_target(
    rows: &[domain::FileCoverage],
    query: ChapterQuery,
) -> Option<(domain::FileCoverage, domain::UnitKey)> {
    match (query.season, query.episode) {
        (Some(season), Some(episode)) => {
            let unit =
                domain::UnitKey::new(i32::try_from(season).ok()?, i32::try_from(episode).ok()?);
            rows.iter()
                .find(|row| row.units.contains(&unit))
                .map(|row| (row.clone(), unit))
        }
        _ => {
            let candidates = rows
                .iter()
                .filter(|row| query.season.is_none_or(|season| covers_season(row, season)))
                .cloned()
                .collect::<Vec<_>>();
            let row = preferred_coverage(&candidates)?;
            let unit = row
                .units
                .iter()
                .copied()
                .filter(|unit| {
                    query.season.is_none_or(|season| {
                        unit.episode_coordinates()
                            .is_some_and(|coordinates| coordinates.0 == season)
                    })
                })
                .max_by_key(|unit| (unit.season, unit.episode))?;
            Some((row.clone(), unit))
        }
    }
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
    theintrodb: Option<crate::theintrodb::TheIntroDbClient>,
}

async fn capture_target(
    state: &ApiState,
    id: String,
    media_id: domain::MediaId,
    user: Option<domain::UserId>,
    query: ChapterQuery,
) -> Result<ChapterTarget, Response> {
    super::store_wait::store_read(state, move |store| {
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
        let (row, unit) = selected_target(&rows, query).ok_or_else(|| {
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

async fn chapter_items_off_executor(
    state: &ApiState,
    row: domain::FileCoverage,
    chapters: Vec<library::ChapterMarker>,
) -> Result<Vec<Value>, Response> {
    let files = state.file_stats.clone();
    state
        .fs_work
        .reserve(std::time::Duration::from_secs(5))
        .await
        .map_err(|_| super::store_wait::busy_response())?
        .run_to_completion(move || chapter_items(&row, &chapters, files.as_ref()))
        .await
        .map_err(super::library::browse_work_error)
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

fn chapter_items(
    row: &domain::FileCoverage,
    chapters: &[library::ChapterMarker],
    files: &dyn crate::file_stat_read::FileStatRead,
) -> Vec<Value> {
    chapters
        .iter()
        .enumerate()
        .map(|(index, chapter)| {
            let image_url = (row.units.len() == 1
                && files.is_file(&chapter_image_path(&row.file.path, index)))
            .then(|| format!("/chapters/{}/{index}", row.file.file_id));
            let mut obj = json!({
                "start_ms": chapter.start_ms,
                "end_ms": chapter.end_ms,
                "title": chapter.title,
                "image_url": image_url,
            });
            if let Some(mt) = chapter.marker_type {
                let tag = match mt {
                    library::MarkerType::IntroStart => "IntroStart",
                    library::MarkerType::IntroEnd => "IntroEnd",
                    library::MarkerType::CreditsStart => "CreditsStart",
                };
                obj["marker_type"] = json!(tag);
            }
            obj
        })
        .collect()
}

/// POST /libraries/{id}/items/{item_id}/chapters/generate — extract one scene
/// frame per chapter beside the file.
pub(crate) async fn generate_chapters(
    State(state): State<ApiState>,
    Path((id, item_id)): Path<(String, String)>,
    Query(query): Query<ChapterQuery>,
) -> Response {
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
        theintrodb: theintrodb_client,
        ..
    } = match capture_target(&state, id, media_id, None, query).await {
        Ok(target) => target,
        Err(response) => return response,
    };
    if row.units.len() != 1 {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "chapters.unit_time_mapping_unsupported",
            "多单元文件缺少分集时间映射",
        );
    }
    let path = row.file.path.clone();
    let probe_lease = lease.clone();
    let reservation = match state
        .fs_work
        .reserve(std::time::Duration::from_secs(5))
        .await
    {
        Ok(reservation) => reservation,
        Err(_) => return super::store_wait::busy_response(),
    };
    let mut targets = match reservation
        .run_to_completion({
            let path = path.clone();
            move || probe_chapter_targets(&path, probe_lease.control())
        })
        .await
    {
        Ok(targets) => targets.unwrap_or_default(),
        Err(error) => return super::library::browse_work_error(error),
    };

    if targets.is_empty() {
        let resolved = crate::marker_resolver::resolve_item_chapters(
            &state,
            &row,
            unit,
            &media,
            theintrodb_client,
        )
        .await;
        targets = resolved.iter().map(|c| (c.start_ms, c.end_ms)).collect();
    }

    if targets.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "chapters.none",
            "该文件没有内嵌章节且无可用片头片尾标记",
        );
    }

    let total = targets.len();
    let worker = state.clone();
    let queued = state
        .http_work
        .run_to_completion(move || {
            let _lease = lease;
            worker.require_writable()?;
            worker.require_write_admitted()?;
            worker
                .chapter_frames
                .enqueue_path(&path, targets)
                .map_err(|error| {
                    tracing::warn!(%error, path=%path.display(), "章节帧任务入队失败");
                    err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "chapters.enqueue_failed",
                        "章节帧任务入队失败，请稍后重试",
                    )
                })
        })
        .await;
    match queued {
        Ok(Ok(_)) => {}
        Ok(Err(response)) => return response,
        Err(error) => return super::library::browse_work_error(error),
    }
    ok(json!({ "generated": 0, "total": total, "queued": true })).into_response()
}

/// GET /chapters/{ledger_id}/{index} — a generated chapter scene frame.
pub(crate) async fn chapter_image(
    State(state): State<ApiState>,
    Path((ledger_id, index)): Path<(String, String)>,
) -> Response {
    let Ok(file_id) = domain::LedgerId::from_str(&ledger_id) else {
        return err(StatusCode::NOT_FOUND, "chapters.missing", "章节帧不存在");
    };
    let row = match super::store_wait::store_read(&state, move |store| {
        store.file_coverage_by_id(file_id)
    })
    .await
    {
        Ok(Ok(Some(row))) => row,
        Err(error) => return error.response(),
        _ => return err(StatusCode::NOT_FOUND, "chapters.missing", "章节帧不存在"),
    };
    let index: usize = match index.parse() {
        Ok(index) => index,
        Err(_) => return err(StatusCode::BAD_REQUEST, "chapters.invalid", "章节下标无效"),
    };
    let frame = chapter_image_path(&row.file.path, index);
    let reservation = match state
        .fs_work
        .reserve(std::time::Duration::from_secs(5))
        .await
    {
        Ok(reservation) => reservation,
        Err(_) => return super::store_wait::busy_response(),
    };
    match reservation
        .run_to_completion(move || std::fs::read(frame))
        .await
    {
        Ok(Ok(bytes)) => {
            ([(axum::http::header::CONTENT_TYPE, "image/jpeg")], bytes).into_response()
        }
        Err(error) => super::library::browse_work_error(error),
        _ => err(StatusCode::NOT_FOUND, "chapters.missing", "章节帧不存在"),
    }
}

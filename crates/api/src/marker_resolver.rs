//! Resolver for episode/movie markers without holding the global Store lock
//! across ffprobe, HTTP, or audio fingerprint work.

use domain::{FileCoverage, Media, MediaKind, UnitKey};
use marker::{ChapterMarker, MarkerType, annotate_chapters, probe_chapters};

use crate::management::ApiState;
use crate::store::StoredMediaMarker;
use crate::theintrodb::TheIntroDbClient;

pub trait ChapterProbe: Send + Sync {
    fn probe(&self, path: &std::path::Path) -> Option<Vec<marker::Chapter>>;
}

pub(crate) struct FfprobeChapters;

impl ChapterProbe for FfprobeChapters {
    fn probe(&self, path: &std::path::Path) -> Option<Vec<marker::Chapter>> {
        probe_chapters(path)
    }
}

#[derive(Clone)]
struct ResolveSnapshot {
    row: FileCoverage,
    unit: UnitKey,
    media: Media,
    /// 章节解析缓存：None = 从未探测；Some(任意) = 已有结果（含空结果），
    /// 命中后直接返回，避免无章节媒体每次播放都重跑 ffprobe / 声纹 / 云端。
    cached: Option<Vec<ChapterMarker>>,
    existing_marker: Option<StoredMediaMarker>,
    duration_ms: Option<i64>,
    detect_intros: bool,
    theintrodb: Option<TheIntroDbClient>,
    probe: std::sync::Arc<dyn ChapterProbe>,
}

struct ResolveOutput {
    chapters: Vec<ChapterMarker>,
    markers: Vec<StoredMediaMarker>,
    cacheable: bool,
}

pub(crate) fn marker_coordinates(
    kind: MediaKind,
    unit: UnitKey,
    unit_count: usize,
) -> Option<(u32, u32)> {
    unit.episode_coordinates().or_else(|| {
        (kind == MediaKind::Movie && unit.is_whole() && unit_count == 1).then_some((1, 1))
    })
}

pub(crate) fn unit_marker(
    store: &crate::Store,
    media: &Media,
    unit: UnitKey,
    unit_count: usize,
) -> Option<StoredMediaMarker> {
    let (season, episode) = marker_coordinates(media.kind, unit, unit_count)?;
    store
        .get_media_marker(media.id, Some(season), Some(episode))
        .ok()
        .flatten()
}

#[derive(Debug, thiserror::Error)]
pub enum ChapterRefreshError {
    #[error(
        "unit_time_mapping_unsupported: automatic multiunit chapter refresh requires unit time mapping"
    )]
    UnitTimeMappingUnsupported,
}

/// Clear persisted results for one mapped unit, then resolve it again.
pub async fn force_refresh_item_chapters(
    state: &ApiState,
    row: &FileCoverage,
    unit: UnitKey,
    media: &Media,
    theintrodb: Option<TheIntroDbClient>,
) -> Result<Vec<ChapterMarker>, ChapterRefreshError> {
    if row.units.len() != 1 || !row.units.contains(&unit) {
        tracing::error!(file_id = %row.file.file_id, ?unit,
            error_kind = "unit_time_mapping_unsupported", "chapter refresh requires unit time mapping");
        return Err(ChapterRefreshError::UnitTimeMappingUnsupported);
    }
    {
        let store = state.store.lock();
        let _ = store.clear_cached_chapters(&row.file.file_id.to_string());
        if let Some((season, episode)) = marker_coordinates(media.kind, unit, row.units.len()) {
            let locked = unit_marker(&store, media, unit, row.units.len())
                .is_some_and(|marker| marker.locked);
            if !locked {
                let _ = store.delete_media_marker(media.id, Some(season), Some(episode));
            }
        }
    }
    Ok(resolve_item_chapters(state, row, unit, media, theintrodb).await)
}

/// Return persisted markers when source detection is disabled, but do not run
/// embedded chapter or TheIntroDB lookup in this synchronous resolver. Audio
/// fingerprint extraction runs independently in the background probe queue.
pub async fn resolve_item_chapters(
    state: &ApiState,
    row: &FileCoverage,
    unit: UnitKey,
    media: &Media,
    theintrodb: Option<TheIntroDbClient>,
) -> Vec<ChapterMarker> {
    let _lease = match state.acquire_path_work(&row.file.path, media.kind) {
        Ok(lease) => lease,
        Err(_) => return Vec::new(),
    };
    if !row.units.contains(&unit) {
        tracing::error!(file_id = %row.file.file_id, ?unit, "requested chapter unit is not covered by file");
        return Vec::new();
    }
    let snapshot = snapshot(state, row, unit, media, theintrodb);
    if let Some(mut cached) = snapshot.cached.clone() {
        append_marker_chapters(
            &mut cached,
            snapshot.existing_marker.as_ref(),
            snapshot.duration_ms,
        );
        return cached;
    }
    let cache_key = row.file.file_id.to_string();
    let output = tokio::task::spawn_blocking(move || resolve_without_store(snapshot))
        .await
        .unwrap_or_else(|error| {
            tracing::error!(%error, "章节标记解析任务失败");
            ResolveOutput {
                chapters: Vec::new(),
                markers: Vec::new(),
                cacheable: false,
            }
        });
    let mut chapters = output.chapters;
    if crate::fingerprint_job::source::observe_source(row).is_err() {
        return Vec::new();
    }
    {
        let store = state.store.lock();
        if crate::fingerprint_job::source::ensure_source_recorded(&store, row).is_err() {
            return Vec::new();
        }
        for marker in &output.markers {
            let _ = store.put_media_marker(marker);
        }
        if output.cacheable {
            let _ = store.put_cached_chapters(&cache_key, &chapters);
        }
        if let Some(latest) = unit_marker(&store, media, unit, row.units.len()) {
            let duration_ms = store
                .file_coverage_by_id(row.file.file_id)
                .ok()
                .flatten()
                .filter(|current| current.file.version == row.file.version)
                .and_then(|current| current.file.duration_ms);
            append_marker_chapters(&mut chapters, Some(&latest), duration_ms);
            chapters.sort_by_key(|chapter| chapter.start_ms);
        }
    }
    chapters
}

fn snapshot(
    state: &ApiState,
    row: &FileCoverage,
    unit: UnitKey,
    media: &Media,
    theintrodb: Option<TheIntroDbClient>,
) -> ResolveSnapshot {
    let store = state.store.lock();
    let cached = store
        .get_cached_chapters(&row.file.file_id.to_string())
        .ok()
        .flatten();
    let existing_marker = unit_marker(&store, media, unit, row.units.len());
    let duration_ms = store
        .file_coverage_by_id(row.file.file_id)
        .ok()
        .flatten()
        .filter(|current| current.file.version == row.file.version)
        .and_then(|current| current.file.duration_ms);
    let library = crate::http::playback::library_id_for(&store, media, &[&row.file])
        .and_then(|id| store.get_library(&id).ok().flatten());
    let detect_intros = library.as_ref().is_none_or(|library| library.detect_intros);
    // 同步解析链路不做声纹比对（无论 STRM 还是本地文件）：远程/本地音频
    // 提取都是重操作，不能阻塞详情页/播放器请求。声纹统一由**后台异步探测
    // 任务**（ProbeManager）执行——一次拉流同时采 streamdetails 与指纹，
    // 同季 ≥2 集后自动比对识别片头。这里同步链路只走内嵌章节 + TheIntroDB。
    // 仅构造快照，杜绝在每次查询缓存时都刷屏输出「开始解析」日志。
    ResolveSnapshot {
        row: row.clone(),
        unit,
        media: media.clone(),
        cached,
        existing_marker,
        duration_ms,
        detect_intros,
        theintrodb,
        probe: state.chapter_probe.clone(),
    }
}

fn resolve_without_store(snapshot: ResolveSnapshot) -> ResolveOutput {
    let mut markers = Vec::new();
    let mut chapters = Vec::new();
    let mut marker = snapshot.existing_marker.clone();
    let coordinates =
        marker_coordinates(snapshot.media.kind, snapshot.unit, snapshot.row.units.len());
    tracing::info!(
        media = %snapshot.media.title,
        ?coordinates,
        path = %snapshot.row.file.path.display(),
        "【片头片尾】缓存未命中，后台异步执行单集章节探测"
    );
    // STRM 同步解析不做内嵌章节探测：ffprobe 对远程 URL 每次最多等 30s，
    // 会拖慢 PlaybackInfo/详情页请求。STRM 的片头片尾走 TheIntroDB 云端
    // 与后台探测任务的声纹（不阻塞请求）。
    let is_strm = snapshot
        .row
        .file
        .path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("strm"));
    if is_strm {
        tracing::info!(
            media = %snapshot.media.title,
            path = %snapshot.row.file.path.display(),
            "【片头片尾】STRM 同步解析跳过 ffprobe 章节探测（避免阻塞请求），走 TheIntroDB / 后台声纹"
        );
        if snapshot.detect_intros
            && snapshot.row.units.len() == 1
            && marker.is_none()
            && snapshot.media.kind == MediaKind::Tv
        {
            marker = marker_from_theintrodb(&snapshot);
            if let Some(value) = marker.clone() {
                markers.push(value);
            }
        }
        return ResolveOutput {
            chapters,
            markers,
            cacheable: true,
        };
    }

    let probed = snapshot.probe.probe(&snapshot.row.file.path);
    if let Some(ref probed) = probed {
        chapters = annotate_chapters(probed);
        tracing::info!(
            media = %snapshot.media.title,
            path = %snapshot.row.file.path.display(),
            count = chapters.len(),
            "【片头片尾】内嵌章节探测成功"
        );
    } else {
        tracing::warn!(
            media = %snapshot.media.title,
            path = %snapshot.row.file.path.display(),
            "【片头片尾】内嵌章节探测失败（文件不可达 / 远程超时）"
        );
    }
    let has_intro = chapters
        .iter()
        .any(|chapter| chapter.marker_type == Some(MarkerType::IntroStart));
    if snapshot.detect_intros
        && snapshot.row.units.len() == 1
        && marker.is_none()
        && !has_intro
        && snapshot.media.kind == MediaKind::Tv
    {
        marker = marker_from_theintrodb(&snapshot);
        if let Some(value) = marker.clone() {
            markers.push(value);
        }
    }
    // 声纹比对不在此同步链路执行（避免阻塞请求）：由后台异步探测任务
    // （ProbeManager）采集单集指纹、同季 ≥2 集后自动比对识别片头。
    if snapshot.detect_intros && snapshot.row.units.len() == 1 && marker.is_none() {
        marker = marker_from_chapters(&snapshot, &chapters);
        if let Some(value) = marker.clone() {
            markers.push(value);
        }
    }
    chapters.sort_by_key(|chapter| chapter.start_ms);
    tracing::info!(
        media = %snapshot.media.title,
        path = %snapshot.row.file.path.display(),
        chapters = chapters.len(),
        markers = markers.len(),
        "【片头片尾】解析完成：章节 {} 条，片头片尾标记 {} 条",
        chapters.len(),
        markers.len()
    );
    ResolveOutput {
        chapters,
        markers,
        cacheable: probed.is_some(),
    }
}

fn marker_from_theintrodb(snapshot: &ResolveSnapshot) -> Option<StoredMediaMarker> {
    let client = snapshot.theintrodb.as_ref()?;
    let tmdb_id = snapshot.media.tmdb_id.as_deref()?;
    let (season, episode) = snapshot.unit.episode_coordinates()?;
    match client.get_episode_markers(tmdb_id, season, episode) {
        Ok(Some(result)) => Some(StoredMediaMarker {
            media_id: snapshot.media.id,
            season,
            episode,
            intro_start_ms: result.intro_start_ms,
            intro_end_ms: result.intro_end_ms,
            outro_start_ms: result.outro_start_ms,
            outro_end_ms: result.outro_end_ms,
            source: "theintrodb".into(),
            locked: false,
            updated_at: 0,
        }),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%error, title = %snapshot.media.title, season, episode, "查询 TheIntroDB 失败");
            None
        }
    }
}

fn marker_from_chapters(
    snapshot: &ResolveSnapshot,
    chapters: &[ChapterMarker],
) -> Option<StoredMediaMarker> {
    let intro = chapters
        .iter()
        .find(|chapter| chapter.marker_type == Some(MarkerType::IntroStart));
    let outro = chapters
        .iter()
        .find(|chapter| chapter.marker_type == Some(MarkerType::CreditsStart));
    let (season, episode) =
        marker_coordinates(snapshot.media.kind, snapshot.unit, snapshot.row.units.len())?;
    (intro.is_some() || outro.is_some()).then(|| StoredMediaMarker {
        media_id: snapshot.media.id,
        season,
        episode,
        intro_start_ms: intro.map(|chapter| chapter.start_ms),
        intro_end_ms: intro.map(|chapter| chapter.end_ms),
        outro_start_ms: outro.map(|chapter| chapter.start_ms),
        outro_end_ms: outro.map(|chapter| chapter.end_ms),
        source: "chapter".into(),
        locked: false,
        updated_at: 0,
    })
}

pub(crate) fn append_marker_chapters(
    chapters: &mut Vec<ChapterMarker>,
    marker: Option<&StoredMediaMarker>,
    duration_ms: Option<i64>,
) {
    let Some(marker) = marker else { return };
    let intro = match (marker.intro_start_ms, marker.intro_end_ms) {
        (Some(s), Some(e)) if e > s => Some((s, e)),
        _ => None,
    };
    let outro = match (marker.outro_start_ms, marker.outro_end_ms) {
        (Some(s), Some(e)) if e > s => Some((s, e)),
        (Some(s), None) => Some((s, s + 60_000)),
        _ => None,
    };
    // 自动为无章节视频构造完整时间轴分段（[序幕 ->] 片头 -> 正片 [-> 片尾]）
    *chapters = marker::build_complete_timeline_chapters(chapters, intro, outro, duration_ms);
}

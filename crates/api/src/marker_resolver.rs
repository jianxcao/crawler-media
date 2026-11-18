//! Resolver for episode/movie markers without holding the global Store lock
//! across ffprobe, HTTP, or audio fingerprint work.

use domain::{FileCoverage, Media, MediaKind, UnitKey};
use marker::{ChapterMarker, MarkerType, annotate_chapters, probe_chapters};

use crate::management::ApiState;
use crate::store::StoredFileMarker;
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
    /// Physical chapter cache, including known-empty embedded chapters.
    /// Cloud absence is cached separately with a bounded TTL and full identity.
    cached: Option<Vec<ChapterMarker>>,
    existing_marker: Option<StoredFileMarker>,
    duration_ms: Option<i64>,
    detect_intros: bool,
    theintrodb: Option<TheIntroDbClient>,
    probe: std::sync::Arc<dyn ChapterProbe>,
    negative: std::sync::Arc<crate::theintrodb_lookup::NegativeCache>,
    now_ms: i64,
}

struct ResolveOutput {
    chapters: Vec<ChapterMarker>,
    markers: Vec<StoredFileMarker>,
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
    row: &FileCoverage,
    unit: UnitKey,
) -> Option<StoredFileMarker> {
    store
        .library_read_snapshot(|store| unit_marker_on(store, row, unit))
        .ok()
        .flatten()
}

fn unit_marker_on(
    store: &store::LibraryConnection<'_>,
    row: &FileCoverage,
    unit: UnitKey,
) -> Option<StoredFileMarker> {
    if !row.units.contains(&unit) {
        return None;
    }
    store
        .get_file_marker(row.file.file_id, unit)
        .ok()
        .flatten()
        .filter(|marker| marker.version == row.file.version)
}

#[derive(Debug, thiserror::Error)]
pub enum ChapterRefreshError {
    #[error(
        "unit_time_mapping_unsupported: automatic multiunit chapter refresh requires unit time mapping"
    )]
    UnitTimeMappingUnsupported,
    #[error("chapter refresh is unavailable ({0})")]
    Unavailable(axum::http::StatusCode),
    #[error("chapter source changed: {0}")]
    SourceChanged(String),
    #[error("chapter storage failed: {0}")]
    Storage(String),
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
    state.theintrodb_negative.invalidate(row.file.file_id);
    let expected = row.clone();
    let expected_media = media.clone();
    let worker = state.clone();
    state
        .http_work
        .run_to_completion(move || clear_for_refresh(&worker, &expected, unit, &expected_media))
        .await
        .map_err(|error| {
            tracing::error!(%error, "chapter refresh worker failed");
            ChapterRefreshError::Unavailable(axum::http::StatusCode::SERVICE_UNAVAILABLE)
        })??;
    Ok(resolve_item_chapters(state, row, unit, media, theintrodb).await)
}

fn clear_for_refresh(
    state: &ApiState,
    row: &FileCoverage,
    unit: UnitKey,
    _media: &Media,
) -> Result<(), ChapterRefreshError> {
    let lease = state
        .library_work
        .acquire_bounded(&state.store, &row.file.library_id.to_string())
        .map_err(|error| {
            tracing::warn!(%error, file_id=%row.file.file_id, "chapter refresh lease was rejected");
            ChapterRefreshError::Unavailable(axum::http::StatusCode::CONFLICT)
        })?;
    lease
        .publish(|| {
            Ok(
                crate::http::store_wait::with_foreground_store(state, |store| {
                    store.library_write_with_error(
                        |error| ChapterRefreshError::Storage(error.to_string()),
                        |store| {
                            crate::fingerprint_job::source::ensure_source_recorded_on(store, row)
                                .map_err(ChapterRefreshError::SourceChanged)?;
                            store
                                .clear_cached_chapters(&row.file.file_id.to_string())
                                .map_err(|error| ChapterRefreshError::Storage(error.to_string()))?;
                            if !unit_marker_on(store, row, unit).is_some_and(|marker| marker.locked)
                            {
                                store.delete_file_marker(row.file.file_id, unit).map_err(
                                    |error| ChapterRefreshError::Storage(error.to_string()),
                                )?;
                            }
                            Ok(())
                        },
                    )
                })
                .map_err(|response| ChapterRefreshError::Unavailable(response.status()))
                .and_then(std::convert::identity),
            )
        })
        .map_err(ChapterRefreshError::SourceChanged)?
}

/// Return current file-local chapter markers; enabled TheIntroDB lookups run
/// without audio reads and publish through the same source/lifecycle barriers.
pub async fn resolve_item_chapters(
    state: &ApiState,
    row: &FileCoverage,
    unit: UnitKey,
    media: &Media,
    theintrodb: Option<TheIntroDbClient>,
) -> Vec<ChapterMarker> {
    let reservation = match state
        .chapter_work
        .reserve(std::time::Duration::from_secs(5))
        .await
    {
        Ok(reservation) => reservation,
        Err(error) => {
            tracing::warn!(%error, file_id=%row.file.file_id, "chapter resolver capacity unavailable");
            return Vec::new();
        }
    };
    resolve_item_chapters_reserved(state, row, unit, media, theintrodb, reservation).await
}

pub(crate) async fn resolve_item_chapters_reserved(
    state: &ApiState,
    row: &FileCoverage,
    unit: UnitKey,
    media: &Media,
    theintrodb: Option<TheIntroDbClient>,
    reservation: crate::blocking_budget::Reservation,
) -> Vec<ChapterMarker> {
    let state = state.clone();
    let row = row.clone();
    let media = media.clone();
    match reservation
        .run_to_completion(move || {
            resolve_item_chapters_blocking(&state, &row, unit, &media, theintrodb)
        })
        .await
    {
        Ok(chapters) => chapters,
        Err(error) => {
            tracing::error!(%error, "chapter resolver worker failed");
            Vec::new()
        }
    }
}

fn resolve_item_chapters_blocking(
    state: &ApiState,
    row: &FileCoverage,
    unit: UnitKey,
    media: &Media,
    theintrodb: Option<TheIntroDbClient>,
) -> Vec<ChapterMarker> {
    let lease = match state
        .library_work()
        .acquire_bounded(&state.store, &row.file.library_id.to_string())
    {
        Ok(lease) => lease,
        Err(_) => return Vec::new(),
    };
    if !row.units.contains(&unit) {
        tracing::error!(file_id = %row.file.file_id, ?unit, "requested chapter unit is not covered by file");
        return Vec::new();
    }
    let Some(snapshot) = snapshot(state, row, unit, media, theintrodb) else {
        return Vec::new();
    };
    if let Some(mut cached) = snapshot
        .cached
        .clone()
        .filter(|_| !cloud_lookup_needed(&snapshot))
    {
        append_marker_chapters(
            &mut cached,
            snapshot.existing_marker.as_ref(),
            snapshot.duration_ms,
        );
        return cached;
    }
    let mut output = resolve_without_store(&snapshot);
    let mut chapters = std::mem::take(&mut output.chapters);
    if crate::fingerprint_job::source::observe_source(row).is_err() {
        return Vec::new();
    }
    let members = vec![store::SourceObservation {
        file_id: row.file.file_id,
        expected: row.file.version.clone(),
        presence: domain::FilePresence::Present,
    }];
    let publication = lease.publish(|| {
        Ok(crate::http::store_wait::with_foreground_store(
            state,
            |store| {
                store.library_write_with_error(
                    |error| error.to_string(),
                    |store| {
                        publish_resolved(store, &snapshot, &mut output, &mut chapters, &members)
                    },
                )
            },
        ))
    });
    let publication = match publication {
        Ok(publication) => publication,
        Err(error) => {
            tracing::warn!(%error, file_id=%row.file.file_id, "chapter lifecycle publication rejected");
            return Vec::new();
        }
    };
    match publication {
        Ok(Ok(())) => state.chapter_frames.notify_pending(),
        Ok(Err(error)) => {
            tracing::warn!(%error, file_id=%row.file.file_id, "chapter publication rejected");
            return Vec::new();
        }
        Err(response) => {
            tracing::warn!(status=%response.status(), file_id=%row.file.file_id, "chapter publication not admitted");
        }
    }
    if let Some(latest) = unit_marker(&state.store, row, unit) {
        let duration_ms = state
            .store
            .file_coverage_by_id(row.file.file_id)
            .ok()
            .flatten()
            .filter(|current| current.file.version == row.file.version)
            .and_then(|current| current.file.duration_ms);
        append_marker_chapters(&mut chapters, Some(&latest), duration_ms);
        chapters.sort_by_key(|chapter| chapter.start_ms);
    }
    chapters
}

fn publish_resolved(
    store: &store::LibraryConnection<'_>,
    snapshot: &ResolveSnapshot,
    output: &mut ResolveOutput,
    chapters: &mut Vec<ChapterMarker>,
    members: &[store::SourceObservation],
) -> Result<(), String> {
    let row = &snapshot.row;
    let unit = snapshot.unit;
    crate::fingerprint_job::source::ensure_source_recorded_on(store, row)?;
    let cloud = output
        .markers
        .iter()
        .any(|marker| marker.source == "theintrodb");
    if cloud {
        let current = store
            .file_coverage_by_id(row.file.file_id)
            .map_err(|error| error.to_string())?;
        if current.is_none_or(|current| current.file.duration_ms != snapshot.duration_ms) {
            return Err("cloud marker duration changed during lookup".into());
        }
        if !cloud_identity_matches(store, row, &snapshot.media)
            .map_err(|error| error.to_string())?
        {
            return Err("cloud marker Media identity changed during lookup".into());
        }
    }
    let current_chapters = store
        .get_cached_chapters(&row.file.file_id.to_string())
        .map_err(|error| error.to_string())?;
    if current_chapters != snapshot.cached
        && let Some(current) = current_chapters
    {
        *chapters = current;
        if cloud && let Some(marker) = marker_from_chapter_ranges(row, unit, chapters) {
            output.markers = vec![marker];
        }
    }
    // Publications made during HTTP keep their priority, including a new manual lock.
    let markers = if unit_marker_on(store, row, unit).is_none() {
        output.markers.as_slice()
    } else {
        &[]
    };
    store
        .publish_file_chapters_checked(
            row,
            unit,
            members,
            markers,
            output.cacheable.then_some(chapters.as_slice()),
        )
        .map_err(|error| error.to_string())
}

fn snapshot(
    state: &ApiState,
    row: &FileCoverage,
    unit: UnitKey,
    media: &Media,
    theintrodb: Option<TheIntroDbClient>,
) -> Option<ResolveSnapshot> {
    let store = state.store.clone();
    if let Err(error) = crate::fingerprint_job::source::ensure_source_recorded(&store, row) {
        tracing::warn!(%error, file_id=%row.file.file_id, "discarded stale chapter snapshot before cache lookup");
        return None;
    }
    let cached = store
        .get_cached_chapters(&row.file.file_id.to_string())
        .ok()
        .flatten();
    let existing_marker = unit_marker(&store, row, unit);
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
    Some(ResolveSnapshot {
        row: row.clone(),
        unit,
        media: media.clone(),
        cached,
        existing_marker,
        duration_ms,
        detect_intros,
        theintrodb,
        probe: state.chapter_probe.clone(),
        negative: state.theintrodb_negative.clone(),
        now_ms: crate::playback_events::PlaybackClock::now_ms(state.playback_clock.as_ref()),
    })
}

fn cloud_lookup_needed(snapshot: &ResolveSnapshot) -> bool {
    snapshot.detect_intros
        && snapshot.row.units.len() == 1
        && snapshot.existing_marker.is_none()
        && snapshot.media.kind == MediaKind::Tv
        && snapshot.theintrodb.is_some()
        && snapshot.media.tmdb_id.is_some()
        && snapshot.unit.episode_coordinates().is_some()
        && snapshot
            .cached
            .as_ref()
            .is_none_or(|chapters| marker_from_chapters(snapshot, chapters).is_none())
}

fn cloud_identity_matches(
    store: &store::LibraryConnection<'_>,
    row: &FileCoverage,
    expected: &Media,
) -> Result<bool, store::StoreError> {
    let Some(current) = store.file_coverage_by_id(row.file.file_id)? else {
        return Ok(false);
    };
    if current.file.media_id != expected.id || current.file.version != row.file.version {
        return Ok(false);
    }
    let Some(media) = store.get_media(expected.id)? else {
        return Ok(false);
    };
    Ok(media.kind == expected.kind && media.tmdb_id == expected.tmdb_id)
}

fn resolve_without_store(snapshot: &ResolveSnapshot) -> ResolveOutput {
    let is_strm = snapshot
        .row
        .file
        .path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"));
    let (mut chapters, cacheable) = if let Some(cached) = &snapshot.cached {
        (cached.clone(), true)
    } else if is_strm {
        // Cloud lookup never opens a remote Media stream or extracts audio.
        (Vec::new(), true)
    } else {
        match snapshot.probe.probe(&snapshot.row.file.path) {
            Some(chapters) => (annotate_chapters(&chapters), true),
            None => {
                tracing::warn!(file_id=%snapshot.row.file.file_id, "embedded chapter probe failed");
                (Vec::new(), false)
            }
        }
    };
    let mut markers = Vec::new();
    if snapshot.detect_intros && snapshot.row.units.len() == 1 && snapshot.existing_marker.is_none()
    {
        if let Some(marker) = marker_from_chapters(snapshot, &chapters) {
            markers.push(marker);
        } else if cloud_lookup_needed(snapshot)
            && let Some(marker) = lookup_theintrodb_marker(snapshot)
        {
            markers.push(marker);
        }
    }
    chapters.sort_by_key(|chapter| chapter.start_ms);
    ResolveOutput {
        chapters,
        markers,
        cacheable,
    }
}

/// Adopt a separate cloud source using the selected release's known duration;
/// this performs no file-local audio verification and never reports it as such.
fn lookup_theintrodb_marker(snapshot: &ResolveSnapshot) -> Option<StoredFileMarker> {
    let client = snapshot.theintrodb.as_ref()?;
    let tmdb_id = snapshot.media.tmdb_id.as_deref()?;
    let (season, episode) = snapshot.unit.episode_coordinates()?;
    let key = snapshot
        .duration_ms
        .filter(|duration| *duration > 0)
        .map(|duration_ms| crate::theintrodb_lookup::LookupKey {
            file_id: snapshot.row.file.file_id,
            version: snapshot.row.file.version.clone(),
            unit: snapshot.unit,
            duration_ms,
            tmdb_id: tmdb_id.to_string(),
            configuration: client.cache_scope(),
        });
    if key
        .as_ref()
        .is_some_and(|key| snapshot.negative.contains(key, snapshot.now_ms))
    {
        return None;
    }
    let candidate = match client.get_episode_markers(tmdb_id, season, episode, snapshot.duration_ms)
    {
        Ok(Some(candidate)) => candidate,
        Ok(None) => {
            if let Some(key) = key {
                snapshot.negative.insert(key, snapshot.now_ms);
            }
            return None;
        }
        Err(error) => {
            tracing::warn!(%error, title=%snapshot.media.title, season, episode, "查询 TheIntroDB 失败");
            return None;
        }
    };
    let duration = snapshot.duration_ms.filter(|duration| *duration > 0)?;
    let group_duration = candidate
        .group_duration_ms
        .filter(|duration| *duration > 0)?;
    // v4 selects within five minutes, but silently falls back to the most
    // popular release when no cluster matches. Reject that fallback ourselves.
    if group_duration.abs_diff(duration) > 300_000 {
        tracing::warn!(file_id=%snapshot.row.file.file_id, duration, group_duration,
            "TheIntroDB release duration does not match this file; candidate was not adopted");
        return None;
    }
    let intro = candidate.intro_start_ms.zip(candidate.intro_end_ms);
    let outro = candidate
        .outro_start_ms
        .map(|start| (start, candidate.outro_end_ms.unwrap_or(duration)));
    if intro
        .into_iter()
        .chain(outro)
        .any(|(start, end)| start < 0 || end <= start || end > duration)
        || intro
            .zip(outro)
            .is_some_and(|((_, end), (start, _))| end > start)
    {
        tracing::warn!(file_id=%snapshot.row.file.file_id, "TheIntroDB candidate exceeds this file or has conflicting ranges");
        return None;
    }
    Some(StoredFileMarker {
        file_id: snapshot.row.file.file_id,
        version: snapshot.row.file.version.clone(),
        unit: snapshot.unit,
        intro_start_ms: intro.map(|range| range.0),
        intro_end_ms: intro.map(|range| range.1),
        outro_start_ms: outro.map(|range| range.0),
        outro_end_ms: outro.map(|range| range.1),
        source: "theintrodb".into(),
        locked: false,
        updated_at: 0,
    })
}

fn marker_from_chapters(
    snapshot: &ResolveSnapshot,
    chapters: &[ChapterMarker],
) -> Option<StoredFileMarker> {
    marker_coordinates(snapshot.media.kind, snapshot.unit, snapshot.row.units.len())?;
    marker_from_chapter_ranges(&snapshot.row, snapshot.unit, chapters)
}

fn marker_from_chapter_ranges(
    row: &FileCoverage,
    unit: UnitKey,
    chapters: &[ChapterMarker],
) -> Option<StoredFileMarker> {
    let intro = chapters
        .iter()
        .find(|chapter| chapter.marker_type == Some(MarkerType::IntroStart));
    let outro = chapters
        .iter()
        .find(|chapter| chapter.marker_type == Some(MarkerType::CreditsStart));
    (intro.is_some() || outro.is_some()).then(|| StoredFileMarker {
        file_id: row.file.file_id,
        version: row.file.version.clone(),
        unit,
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
    marker: Option<&StoredFileMarker>,
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

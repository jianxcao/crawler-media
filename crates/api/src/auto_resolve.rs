//! Auto-resolve media metadata against TMDB/Catalog during library scans.
//! Inspired by MovieClaw & MoviePilot two-stage identification.

mod matching;
use matching::{find_catalog_match, search_queries};

use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;
use domain::{Media, MediaKind};

/// Automatically look up TMDB metadata for a media item if it lacks external IDs.
/// If an exact or high-confidence match is found:
/// 1. Updates `media` in the database with `tmdb_id`, `year`, `original_title`
/// 2. Downloads and writes the official poster beside the preferred file
pub fn auto_resolve_media(
    state: &ApiState,
    media: &Media,
    sample_path: &std::path::Path,
) -> Option<Media> {
    resolve_media(state, media, sample_path, true)
}

/// Scans publish sidecars only after rebinding their newly inserted ledger rows.
pub(crate) fn resolve_scanned_media(
    state: &ApiState,
    media: &Media,
    sample_path: &std::path::Path,
) -> Option<Media> {
    resolve_media(state, media, sample_path, false)
}

fn resolve_media(
    state: &ApiState,
    media: &Media,
    sample_path: &std::path::Path,
    publish_sidecars: bool,
) -> Option<Media> {
    let lease = state.acquire_path_work(sample_path, media.kind).ok()?;
    if has_local_scan_metadata(state, media, sample_path)
        || media.tmdb_id.is_some()
        || media.douban_id.is_some()
    {
        return Some(media.clone());
    }

    let title = media.title.trim();
    if title.is_empty() {
        return None;
    }

    tracing::info!(
        title = %title,
        kind = ?media.kind,
        "扫描时自动解析媒体元数据（TMDB/目录）"
    );

    let queries = search_queries(title, sample_path);
    let mut best_hit = None;
    for query in &queries {
        let hits = match media.kind {
            MediaKind::Movie => state.catalog.search_movie(query),
            MediaKind::Tv => state.catalog.search_tv_year(query, media.year),
            MediaKind::Video => continue,
        };
        let hits = match hits {
            Ok(hits) => hits,
            Err(error) => {
                tracing::error!(%error, %query, kind = ?media.kind, "扫描媒体目录搜索失败");
                continue;
            }
        };
        if let Some(hit) = find_catalog_match(state.catalog.as_ref(), media, query, &hits) {
            best_hit = Some(hit);
            break;
        }
    }
    if lease
        .as_ref()
        .is_some_and(|lease| lease.control().is_cancelled())
    {
        return None;
    }
    let Some(best_hit) = best_hit else {
        tracing::debug!(title = %title, year = ?media.year, "扫描时未找到可靠的 TMDB 匹配");
        if media.kind == MediaKind::Tv {
            attach_episode_frames(state, media, sample_path);
        }
        return None;
    };

    let tmdb_id = best_hit.media.tmdb_id.clone()?;
    tracing::info!(
        title = %title,
        matched_title = %best_hit.media.title,
        tmdb_id = %tmdb_id,
        year = ?best_hit.media.year,
        "成功匹配 TMDB 媒体"
    );

    let mut updated = media.clone();
    if updated.tmdb_id.is_none() {
        updated.tmdb_id = best_hit.media.tmdb_id.clone();
    }
    if updated.douban_id.is_none() {
        updated.douban_id = best_hit.media.douban_id.clone();
    }
    if updated.year.is_none() {
        updated.year = best_hit.media.year;
    }
    if updated.original_title.is_none() {
        updated.original_title = best_hit.media.original_title.clone();
    }

    // External IDs are aliases: an online match may already have a canonical
    // Media with Playback history. The scan caller rebinds only its new rows.
    let updated = {
        let store = state.store.lock();
        if lease
            .as_ref()
            .is_some_and(|lease| lease.control().is_cancelled())
        {
            return None;
        }
        match store.resolve_scanned_media(updated) {
            Ok(canonical) => canonical,
            Err(error) => {
                tracing::error!(%error, media_id = %media.id, "无法汇合扫描媒体身份");
                return None;
            }
        }
    };

    if !publish_sidecars {
        return Some(updated);
    }

    // Attach official poster & backdrop
    let _ = crate::poster_fetch::attach_poster(state, &updated, sample_path);
    let _ = crate::poster_fetch::attach_backdrop(state, &updated, sample_path);

    // Write sidecars (NFOs, stills, Fanart) beside video files and series root
    write_resolved_nfo(state, &updated, sample_path);

    // Persist the display facts a Scrape observed (rating, release date, studio, logo)
    // through the adopted metadata; the affected cards are reduced in that transaction.
    if let Some(tmdb_id) = &updated.tmdb_id
        && let Ok(Some(meta)) =
            crate::scrape_metadata::fetch_tmdb_metadata(state, updated.kind, tmdb_id)
    {
        crate::scrape_metadata::adopt_tmdb_metadata(
            state,
            &updated,
            tmdb_id,
            domain::EntityScope::Work,
        );
        crate::scrape_metadata::record_scraped_display(state, updated.id, &meta);
    }
    Some(updated)
}

pub(crate) fn write_resolved_nfo(state: &ApiState, media: &Media, sample_path: &std::path::Path) {
    let lease = match state.acquire_path_work(sample_path, media.kind) {
        Ok(lease) => lease,
        Err(error) => {
            tracing::warn!(path = %sample_path.display(), %error, "自动 NFO 写入被拒绝");
            return;
        }
    };
    let Some(tmdb_id) = media.tmdb_id.as_deref() else {
        return;
    };
    let meta_service =
        crate::metadata_service::MetadataService::new(std::sync::Arc::new(state.clone()));
    let _ = meta_service.refresh_media_metadata(media);

    if media.kind == MediaKind::Tv {
        let (coverages, sample_coverage) = {
            let store = state.store.lock();
            let all = store.file_coverage_for_media(media.id).unwrap_or_default();
            let sample_path_str = sample_path.display().to_string();
            let sample = all
                .iter()
                .find(|coverage| coverage.file.path.display().to_string() == sample_path_str)
                .cloned();
            (all, sample)
        };
        let sample_coverage = match sample_coverage {
            Some(coverage) => Some(coverage),
            None => {
                let (season, episode) = sample_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|name| {
                        let parsed = release::parse(name);
                        (parsed.season, parsed.episode)
                    })
                    .unwrap_or((None, None));
                synthetic_coverage(state, media.id, media.kind, sample_path, season, episode)
            }
        };
        // A path no Library owns has no sidecar to write: writing one would land
        // outside every Library root, and inventing an owner would let a
        // not-yet-published file pass ownership filters it must not pass.
        let Some(sample_coverage) = sample_coverage else {
            tracing::warn!(
                path = %sample_path.display(),
                media_id = %media.id,
                "样例文件不属于任何 Library，跳过 TV sidecar 写入"
            );
            return;
        };
        let show_root = crate::scrape_metadata::show_root(sample_path, &sample_coverage);
        let coverages_to_write = if coverages.is_empty() {
            vec![sample_coverage]
        } else {
            coverages
        };
        crate::scrape_metadata::scrape_tv_sidecars(state, media, &show_root, &coverages_to_write);
    } else {
        let nfo_enabled = state
            .store
            .lock()
            .get_scrape_config()
            .ok()
            .map(|config| config.effective.mirror_nfo)
            .unwrap_or(true);
        if nfo_enabled
            && let Ok(Some(metadata)) =
                crate::scrape_metadata::fetch_tmdb_metadata(state, media.kind, tmdb_id)
        {
            let nfo = crate::scrape_metadata::nfo_from_tmdb(media, &metadata);
            if let Some(stem) = sample_path.file_stem().and_then(|value| value.to_str()) {
                let target = sample_path.with_file_name(format!("{stem}.nfo"));
                let write = || {
                    crate::scrape_metadata::write_nfo(&target, media, &nfo);
                    Ok(())
                };
                let _ = match &lease {
                    Some(lease) => lease.publish(write),
                    None => write(),
                };
            }
        }
        let movie_root = sample_path.parent().unwrap_or(sample_path);
        crate::scrape_metadata::scrape_movie_fanart(state, media, movie_root);
    }
}

/// 样例文件在台账里查无行时的占位覆盖：只携带文件名解析出的集坐标与路径。
///
/// 仅用于 `show_root` 的整片/分集判定与 `scrape_tv_sidecars` 的按路径取租约，
/// 不写库、不代表任何已发布的文件版本。季节与集号两者都有才算分集；
/// 只有一个（旧模型允许 `(Some, None)` 这种半坐标）时按整片处理，绝不造 `(0, 0)`。
///
/// **归属必须是真的**：`library_id`/`root_id` 来自 Store 唯一的 path→Library 规则
/// （`library_for_path_strict`）。下游按 `coverage.file.library_id` 过滤要写的
/// sidecar，所以 `LibraryId::new()` 这种「造一个没人认领的 id」会让一份合法的
/// 匹配被静默丢掉。路径确实不属于任何 Library 时返回 `None`，调用方跳过写入。
fn synthetic_coverage(
    state: &ApiState,
    media_id: domain::MediaId,
    kind: domain::MediaKind,
    path: &std::path::Path,
    season: Option<u32>,
    episode: Option<u32>,
) -> Option<domain::FileCoverage> {
    let (library_id, root_id) = {
        let store = state.store.lock();
        let library = store.library_for_path_strict(path, kind).ok().flatten()?;
        // The root is stored with the operator's spelling while the sample path may
        // be spelled differently (`/var` vs `/private/var`): compare resolved paths,
        // never raw prefixes.
        let root_id = store
            .list_library_roots()
            .unwrap_or_default()
            .into_iter()
            .filter(|root| {
                root.library_id == library.id && crate::store::path_under_root(path, &root.path)
            })
            .max_by_key(|root| root.path.components().count())
            .map(|root| root.id)
            .unwrap_or_default();
        (library.id.parse::<domain::LibraryId>().ok()?, root_id)
    };
    let units = match (season, episode) {
        (Some(season), Some(episode)) => vec![domain::UnitKey::new(season as i32, episode as i32)],
        _ => vec![domain::UnitKey::WHOLE],
    };
    Some(domain::FileCoverage {
        path_key: String::new(),
        file: domain::LibraryFile {
            file_id: domain::LedgerId::new(),
            library_id,
            root_id,
            media_id,
            kind,
            path: path.to_path_buf(),
            version: domain::FileVersion {
                revision: 1,
                source_version: crate::fingerprint_job::current_source_version(path),
            },
            presence: domain::FilePresence::Present,
            publication: domain::FilePublication::Published,
            size_bytes: None,
            stat_known: false,
            created_ns: None,
            modified_ns: None,
            source_digest: None,
            source_path: None,
            transfer_mode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: None,
            tracks_version: 0,
            poster_path: None,
            backdrop_path: None,
            still_path: None,
        },
        units,
    })
}

/// 目录对不上时，给同一部剧还没有剧照的分集各截一帧。
/// STRM 走里面的远程地址，和流探测同一套输入，不把 `.strm` 文本当视频。
fn attach_episode_frames(state: &ApiState, media: &Media, sample_path: &std::path::Path) {
    let Some(dir) = sample_path.parent() else {
        return;
    };
    // Pure read: a read-only Store and a directory-range query, never the writer
    // lock or a whole-Library ledger scan (L1a).
    let paths = state
        .store_readers
        .checkout(crate::http::store_wait::STORE_READ_WAIT)
        .and_then(|store| store.file_media_under(dir).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|(media_id, path)| *media_id == media.id && path.parent() == Some(dir))
        .map(|(_, path)| path)
        .collect::<Vec<_>>();
    let targets = if paths.is_empty() {
        vec![sample_path.to_path_buf()]
    } else {
        paths
    };
    for path in targets {
        let lease = match state.acquire_path_work(&path, media.kind) {
            Ok(lease) => lease,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "分集抓帧被拒绝");
                continue;
            }
        };
        let control = lease
            .as_ref()
            .map(|lease| lease.control().clone())
            .unwrap_or_default();
        let still = crate::episode_still::path(&path);
        if still.is_file() {
            continue;
        }
        if let Err(error) = library::extract_frame_controlled(&path, 60_000, &still, &control) {
            tracing::warn!(path = %path.display(), %error, "匹配失败后分集抓帧失败");
        }
    }
}

/// Complete local identity and a display title are enough; missing plot/cast
/// never implicitly turns a normal scan into an explicit metadata refresh.
pub(crate) fn has_local_scan_metadata(
    state: &ApiState,
    media: &domain::Media,
    path: &std::path::Path,
) -> bool {
    let root = {
        let store = state.store.lock();
        store
            .library_for_path_strict(path, media.kind)
            .ok()
            .flatten()
            .and_then(|library| {
                // Recorded spelling vs the caller's spelling: resolve both sides.
                library
                    .root_paths
                    .into_iter()
                    .find(|root| crate::store::path_under_root(path, root))
            })
    };
    let Some(root) = root else { return false };
    match library::read_watch_identity(path, &root, media.kind) {
        Ok(Some(identity)) => {
            identity.title.is_some()
                && [
                    identity.tmdb_id,
                    identity.douban_id,
                    identity.tvdb_id,
                    identity.bangumi_id,
                    identity.anilist_id,
                ]
                .iter()
                .any(Option::is_some)
        }
        Ok(None) => false,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "无法读取本地 NFO，回退扫描识别");
            false
        }
    }
}

pub(crate) fn rebind_scan_rows(
    state: &ApiState,
    previous: domain::MediaId,
    canonical: domain::MediaId,
    inserted: &[std::path::PathBuf],
) -> Result<(), String> {
    if previous == canonical {
        return Ok(());
    }
    let store = state.store.lock();
    for path in inserted {
        let Some(coverage) = store
            .file_coverage_by_path(&path.display().to_string())
            .map_err(|e| e.to_string())?
            .filter(|coverage| coverage.file.media_id == previous)
        else {
            continue;
        };
        let kind = store
            .get_media(canonical)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "canonical Media missing".to_string())?
            .kind;
        if let Some(owner) = store
            .library_for_path_strict(path, kind)
            .map_err(|e| e.to_string())?
        {
            if !store
                .library_accepts_work(&owner.id)
                .map_err(|e| e.to_string())?
            {
                return Err("Library 正在删除".into());
            }
        } else if store
            .path_in_removal_scope(path)
            .map_err(|e| e.to_string())?
        {
            return Err("Library 正在删除，拒绝重绑未归属的旧目录文件".into());
        }
        store
            .rebind_scanned_ledger(coverage.file.file_id, canonical)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

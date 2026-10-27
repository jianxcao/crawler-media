use serde_json::{Value, json};
use std::path::{Path as FsPath, PathBuf};

use crate::store::{Library, LibraryRemoval};

/// Files whose recorded path falls under any root of the library.
pub(crate) fn files_in_library(
    store: &crate::Store,
    library: &Library,
) -> Vec<domain::LibraryFile> {
    coverages_in_library(store, library)
        .into_iter()
        .map(|coverage| coverage.file)
        .collect()
}

/// 同 [`files_in_library`] 的过滤，但保留每个文件的 unit 覆盖（`FileCoverage`）。
pub(crate) fn coverages_in_library(
    store: &crate::Store,
    library: &Library,
) -> Vec<domain::FileCoverage> {
    let Ok(id) = library.id.parse() else {
        return Vec::new();
    };
    store
        .file_coverage_in_library(id, None, store::FileReadScope::AllRecorded)
        .unwrap_or_default()
}

/// 封面决策的路径输入：只认显式配置的路径。
struct CoverInput {
    disabled: bool,
    explicit: Option<PathBuf>,
}

/// 一个 Library 在**锁内**收集到的 owned DB 事实。
///
/// 这里刻意不含任何文件系统观察：`stat`/`is_dir`/`is_file` 在锁外（受 blocking
/// 预算约束）才做，慢挂载不会阻塞共享 Store。统计（计数/体积/刷新状态）也不再
/// 在这里逐库重算：由 [`crate::library_statistics`] 一次聚合提供。
pub(crate) struct LibraryFacts {
    pub(crate) library: Library,
    removal: Option<LibraryRemoval>,
    root_reassignments: Vec<store::RootReassignment>,
    cover: CoverInput,
}

/// 锁内读取 removal 状态和显式封面路径；不再探测固定命名的旧封面文件。
pub(crate) fn library_facts(store: &crate::Store, library: &Library) -> LibraryFacts {
    let cover = CoverInput {
        disabled: matches!(library.cover_path.as_deref(), Some("none") | Some("")),
        explicit: library
            .cover_path
            .as_deref()
            .and_then(|path| (!path.is_empty() && path != "none").then(|| PathBuf::from(path))),
    };
    LibraryFacts {
        library: library.clone(),
        removal: store.removal_for_library(&library.id).ok().flatten(),
        root_reassignments: store.root_reassignments_for_library(&library.id).unwrap_or_else(|error| {
            tracing::error!(library_id=library.id,%error,"Failed to read root reassignment lifecycle");
            Vec::new()
        }),
        cover,
    }
}

/// Explicit configured covers only; a fixed cover filename is not a storage fact.
fn cover_path_from_input(cover: &CoverInput) -> Option<PathBuf> {
    if cover.disabled {
        return None;
    }
    cover
        .explicit
        .as_ref()
        .filter(|path| path.is_file())
        .cloned()
}

/// 锁外：在 owned 事实之上做文件观察并渲染响应 JSON。
///
/// `stats` 由 [`crate::library_statistics`] 一次聚合给出（计数来自台账、体积来自
/// 投影 facts、刷新状态来自 head）；这里只负责逐 root `is_dir` 与封面 `is_file`。
pub(crate) fn library_json_from_facts(
    facts: &LibraryFacts,
    stats: Option<crate::library_statistics::LibraryStatistics>,
) -> Value {
    let stats = stats.unwrap_or(crate::library_statistics::LibraryStatistics {
        stale: true,
        ..Default::default()
    });
    let library = &facts.library;
    // 根目录被删：库不能假装是「空的」，要在 UI 提示并提供恢复/删除。
    let root_missing = library
        .root_paths
        .iter()
        .any(|root| !FsPath::new(root).is_dir());
    json!({
        "id": library.id,
        "removal": facts.removal,
        "root_reassignments": facts.root_reassignments,
        "kind": library.kind.as_str(),
        "name": library.name,
        "root_paths": library.root_paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "root_missing": root_missing,
        "is_default": library.is_default,
        "access_mode": library.access_mode,
        "admin_visible": library.admin_visible,
        "member_ids": library.member_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
        "match_rules": library.match_rules,
        "default_filter_id": library.default_filter_id,
        "detect_intros": library.detect_intros,
        "enable_fingerprint": library.enable_fingerprint,
        "realtime_watch": library.realtime_watch,
        "generate_thumbnails": library.generate_thumbnails,
        "extract_chapter_images": library.extract_chapter_images,
        "exclude_from_home": library.exclude_from_home,
        "auto_series_collections": library.auto_series_collections,
        "cover_url": cover_path_from_input(&facts.cover).map(|_| format!("/libraries/{}/cover", library.id)),
        "stats": {
            "item_count": stats.item_count,
            "file_count": stats.file_count,
            "total_size_bytes": stats.total_size_bytes,
            "strm_file_count": stats.strm_file_count,
            "refreshed_at_ms": stats.refreshed_at_ms,
            "stale": stats.stale,
        },
    })
}

/// 同步版本：库配置的写路径仍在锁内直接渲染（不在本次改动范围内），统计用
/// [`crate::library_statistics::statistics_for`] 的同一套口径同步算一次，不构建。
pub(crate) fn library_json(
    store: &crate::Store,
    library: &Library,
) -> Result<Value, store::StoreError> {
    let stats = crate::library_statistics::statistics_for(store, library)?;
    Ok(library_json_from_facts(
        &library_facts(store, library),
        Some(stats),
    ))
}

/// Emby-style current cover source: latest TV episode wins; movies use their
/// sole file. This makes a newly imported episode refresh the series cover.
///
/// Key is `(season.unwrap_or(0), episode.unwrap_or(0), path)` over the file's units;
/// a whole file reads as `(0, 0)` (the `(-1, -1)` sentinel clamps), so a multi-unit
/// file picks the same file the max over its unit rows picks.
pub(crate) fn preferred_coverage<R>(covs: &[R]) -> Option<&domain::FileCoverage>
where
    R: std::borrow::Borrow<domain::FileCoverage>,
{
    covs.iter()
        .map(std::borrow::Borrow::borrow)
        .max_by_key(|coverage| {
            let unit = coverage
                .units
                .iter()
                .map(|unit| (unit.season.max(0), unit.episode.max(0)))
                .max()
                .unwrap_or((0, 0));
            (unit.0, unit.1, coverage.file.path.clone())
        })
}

pub(crate) fn poster_path(file: &domain::LibraryFile) -> Option<PathBuf> {
    poster_path_with(file, &crate::file_stat_read::LocalFileStatRead)
}

/// 与 [`poster_path`] 同一套布局，存在性判断走注入的浏览文件 seam。
pub(crate) fn poster_path_with(
    file: &domain::LibraryFile,
    files: &dyn crate::file_stat_read::FileStatRead,
) -> Option<PathBuf> {
    if let Some(poster) = crate::poster::poster_beside(&file.path.to_string_lossy(), files) {
        return Some(poster);
    }
    let file_dir = file.path.parent()?;
    // 兼容两种布局：平铺（文件旁 poster.jpg）与模板整理后
    // （<show>/Season N/file → show 根 poster.jpg，向上找一层）。
    let up = file_dir.parent()?.join("poster.jpg");
    files.is_file(&up).then_some(up)
}

//! 请求作用域：一次 ownership + 可见库 + 作用域内的候选 coverage。
//!
//! 媒体交集（`SeriesId` / 剧 parent）在 SQL 层收敛到索引化的 `file_coverage_for_media`；
//! 库与 folder 只作为附加归属约束，不再为每个 series 重读整库。

use domain::{FileCoverage, LedgerId, LibraryFile, Media, MediaId, MediaKind, UserId};
use media_server::provider::ServerItemQuery;
use media_server::provider::query::{
    folder_id_for_root, series_media_id as protocol_series_media_id,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// 作用域读失败时已经完成的 coverage 数。`rows_read` 原先只活在成功的
/// [`ScopedRead`] 里，后面的 media/playback 读取用 `?` 失败时计数停在 0。
#[derive(Debug)]
pub(super) struct ScopeError {
    pub(super) rows_read: usize,
    message: String,
}

impl std::fmt::Display for ScopeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(formatter)
    }
}

impl From<String> for ScopeError {
    fn from(message: String) -> Self {
        Self {
            rows_read: 0,
            message,
        }
    }
}

pub(super) struct ScopedRead {
    pub(super) ownership: store::LibraryOwnership,
    pub(super) is_admin: bool,
    pub(super) user_id: UserId,
    pub(super) media: HashMap<MediaId, Media>,
    pub(super) coverages: Vec<FileCoverage>,
    pub(super) units: Vec<crate::store::PlaybackUnitSnapshot>,
    pub(super) files: HashMap<LedgerId, LibraryFile>,
    pub(super) requested_library: Option<String>,
    pub(super) folder_root: Option<PathBuf>,
    pub(super) media_scoped: bool,
    pub(super) unresolved: bool,
    pub(super) unknown_parent: bool,
    pub(super) rows_read: usize,
}

pub(super) fn read_scope(
    store: &crate::Store,
    user_id: UserId,
    query: &ServerItemQuery,
) -> Result<ScopedRead, ScopeError> {
    let ownership = store
        .library_ownership()
        .map_err(|error| error.to_string())?;
    let is_admin = store
        .user_role(user_id)
        .map_err(|error| error.to_string())?
        == "admin";
    let libraries = visible_libraries(store, &ownership, user_id, is_admin)?;
    let parent = query.parent_id.as_deref();
    let is_folder_id = parent.is_some_and(|value| value.starts_with("folder_"));
    let library = parent.filter(|id| libraries.iter().any(|library| library.id == *id));
    let folder = if is_folder_id {
        parent.and_then(|value| folder_root_in(value, &libraries))
    } else {
        None
    };
    if is_folder_id && folder.is_none() {
        return Ok(unresolved_scope(ownership, is_admin, user_id));
    }
    let (series_parent, classified_rows) = match (parent, library, folder.as_ref()) {
        (Some(id), None, None) if !is_folder_id => {
            series_media_id(store, user_id, id, &ownership, is_admin)?
        }
        _ => (None, None),
    };
    let series_param = query.series_id.as_deref();
    let media_target: Option<String> = if library.is_some() || is_folder_id {
        series_param.map(str::to_owned)
    } else if let Some(series) = series_param {
        Some(parent.unwrap_or(series).to_string())
    } else {
        series_parent.map(|_| parent.unwrap_or_default().to_string())
    };
    let media_scope = media_target.as_deref().and_then(protocol_series_media_id);
    if media_target.is_some() && media_scope.is_none() {
        return Ok(unresolved_scope(ownership, is_admin, user_id));
    }
    // 已完成的台账读取必须立刻入账：后面的 media/playback 读取用 `?` 失败时，
    // 调用方拿不到 ScopedRead，候选计数会停在 0。分类阶段读过、可见性又拒绝的
    // 行同样算一次读取，即使结果被丢掉。
    let mut rows_read = classified_rows
        .as_ref()
        .map(|coverages| coverages.len())
        .unwrap_or(0);
    let loaded_media = classified_rows
        .as_ref()
        .and_then(|coverages| coverages.first().map(|coverage| coverage.file.media_id));
    let same_media = media_scope.is_some_and(|id| loaded_media == Some(id));
    // 可见分类沿用已加载的行；不可见时只在显式目标就是这部媒体时复用，避免再读一遍。
    let reusable = classified_rows.filter(|_| series_parent.is_some() || same_media);
    let (coverages, requested_library, folder_root) = if let Some(coverages) = reusable {
        (coverages, None, None)
    } else if let Some(media_id) = media_scope {
        let loaded = store
            .file_coverage_for_media(media_id)
            .map_err(|error| error.to_string())?;
        rows_read += loaded.len();
        (loaded, library.map(str::to_owned), folder)
    } else if let Some(library_id) = library {
        let loaded = store
            .ledger_candidates_for_library(&ownership, library_id)
            .map_err(|error| error.to_string())?;
        rows_read += loaded.len();
        (loaded, Some(library_id.to_string()), None)
    } else if is_folder_id {
        let Some(root) = folder else {
            return Ok(unresolved_scope(ownership, is_admin, user_id));
        };
        let owner = libraries
            .iter()
            .find(|library| library.root_paths.iter().any(|path| path == &root));
        let loaded = match owner {
            Some(library) => store
                .ledger_candidates_for_library(&ownership, &library.id)
                .map_err(|error| error.to_string())?,
            None => Vec::new(),
        };
        rows_read += loaded.len();
        (loaded, None, Some(root))
    } else {
        let loaded = store
            .jellyfin_ledger_candidates(&ownership, None)
            .map_err(|error| error.to_string())?;
        rows_read += loaded.len();
        (loaded, None, None)
    };
    let unknown_parent =
        parent.is_some() && library.is_none() && !is_folder_id && media_target.is_none();
    assemble_scope(
        store,
        ownership,
        is_admin,
        user_id,
        ScopeSeed {
            coverages,
            requested_library,
            folder_root,
            media_scoped: media_scope.is_some(),
            unknown_parent,
            rows_read,
        },
    )
}

/// Classify a protocol request without reading any file coverage or Playback rows.
pub(super) fn request_scope(
    store: &crate::Store,
    user_id: UserId,
    query: &ServerItemQuery,
) -> Result<ScopedRead, store::StoreError> {
    let ownership = store.library_ownership()?;
    let is_admin = store.user_role(user_id)? == "admin";
    let libraries: Vec<_> = store
        .list_libraries()?
        .into_iter()
        .filter(|library| {
            if is_admin {
                library.admin_visible
            } else {
                library.access_mode == "everyone" || library.member_ids.contains(&user_id)
            }
        })
        .collect();
    let parent = query.parent_id.as_deref();
    let is_folder = parent.is_some_and(|value| value.starts_with("folder_"));
    let library = parent.filter(|id| libraries.iter().any(|library| library.id == *id));
    let folder = if is_folder {
        parent.and_then(|id| folder_root_in(id, &libraries))
    } else {
        None
    };
    let series_parent = if library.is_none() && !is_folder {
        parent
            .and_then(protocol_series_media_id)
            .map(|id| store.get_media(id))
            .transpose()?
            .flatten()
            .filter(|media| media.kind == MediaKind::Tv)
    } else {
        None
    };
    let target = if library.is_some() || is_folder {
        query.series_id.as_deref()
    } else if query.series_id.is_some() {
        parent.or(query.series_id.as_deref())
    } else if series_parent.is_some() {
        parent
    } else {
        None
    };
    let media_scope = target.and_then(protocol_series_media_id);
    let mut scope = unresolved_scope(ownership, is_admin, user_id);
    scope.unresolved =
        (is_folder && folder.is_none()) || (target.is_some() && media_scope.is_none());
    scope.requested_library = library.map(str::to_owned);
    scope.folder_root = folder;
    scope.media_scoped = media_scope.is_some();
    scope.unknown_parent = parent.is_some() && library.is_none() && !is_folder && target.is_none();
    if let Some(media) = series_parent {
        scope.media.insert(media.id, media);
    }
    if let Some(media_id) = media_scope
        && !scope.media.contains_key(&media_id)
        && let Some(media) = store.get_media(media_id)?
    {
        scope.media.insert(media.id, media);
    }
    Ok(scope)
}

/// 显式一组媒体（续播候选）的作用域：只读这些媒体的台账行，不读整库。
///
/// 与 `read_scope` 的 `SeriesId` 分支同源：行来自索引化的 `file_coverage_for_media`，
/// 可见性仍交给 `coverage_visible`/`build_light_items` 逐个 coverage 判定。
pub(super) fn read_media_scope(
    store: &crate::Store,
    user_id: UserId,
    media_ids: &[MediaId],
) -> Result<ScopedRead, ScopeError> {
    let ownership = store
        .library_ownership()
        .map_err(|error| error.to_string())?;
    let is_admin = store
        .user_role(user_id)
        .map_err(|error| error.to_string())?
        == "admin";
    let mut coverages: Vec<FileCoverage> = Vec::new();
    for media_id in media_ids {
        let loaded = store
            .file_coverage_for_media(*media_id)
            .map_err(|error| error.to_string())?;
        coverages.extend(loaded);
    }
    let rows_read = coverages.len();
    assemble_scope(
        store,
        ownership,
        is_admin,
        user_id,
        ScopeSeed {
            coverages,
            requested_library: None,
            folder_root: None,
            media_scoped: true,
            unknown_parent: false,
            rows_read,
        },
    )
}

/// 作用域内的 coverage 已定，补齐媒体、播放单元与已发布文件。
struct ScopeSeed {
    coverages: Vec<FileCoverage>,
    requested_library: Option<String>,
    folder_root: Option<PathBuf>,
    media_scoped: bool,
    unknown_parent: bool,
    rows_read: usize,
}

fn assemble_scope(
    store: &crate::Store,
    ownership: store::LibraryOwnership,
    is_admin: bool,
    user_id: UserId,
    seed: ScopeSeed,
) -> Result<ScopedRead, ScopeError> {
    let ScopeSeed {
        coverages,
        requested_library,
        folder_root,
        media_scoped,
        unknown_parent,
        rows_read,
    } = seed;
    let ids: Vec<MediaId> = coverages
        .iter()
        .map(|coverage| coverage.file.media_id)
        .collect();
    let media = store.media_batch(&ids).map_err(|error| {
        tracing::warn!(rows_read, error = %error, "Jellyfin 作用域台账已读完，媒体批量读取失败");
        ScopeError {
            rows_read,
            message: error.to_string(),
        }
    })?;
    let units = store.playback_units_for_media(user_id, &ids).map_err(|error| {
        tracing::warn!(rows_read, error = %error, "Jellyfin 作用域台账已读完，播放单元读取失败");
        ScopeError { rows_read, message: error.to_string() }
    })?;
    let file_ids: Vec<LedgerId> = coverages
        .iter()
        .map(|coverage| coverage.file.file_id)
        .collect();
    let files = store.published_files(&file_ids).map_err(|error| {
        tracing::warn!(rows_read, error = %error, "Jellyfin 作用域台账已读完，文件批量读取失败");
        ScopeError {
            rows_read,
            message: error.to_string(),
        }
    })?;
    Ok(ScopedRead {
        ownership,
        is_admin,
        user_id,
        media,
        coverages,
        units,
        files,
        requested_library,
        folder_root,
        media_scoped,
        unresolved: false,
        unknown_parent,
        rows_read,
    })
}

pub(super) fn unresolved_scope(
    ownership: store::LibraryOwnership,
    is_admin: bool,
    user_id: UserId,
) -> ScopedRead {
    ScopedRead {
        ownership,
        is_admin,
        user_id,
        media: HashMap::new(),
        coverages: Vec::new(),
        units: Vec::new(),
        files: HashMap::new(),
        requested_library: None,
        folder_root: None,
        media_scoped: false,
        unresolved: true,
        unknown_parent: false,
        rows_read: 0,
    }
}

fn visible_libraries(
    store: &crate::Store,
    ownership: &store::LibraryOwnership,
    user_id: UserId,
    is_admin: bool,
) -> Result<Vec<crate::store::Library>, String> {
    let allowed: HashSet<String> = ownership
        .libraries()
        .iter()
        .filter(|library| ownership.accepts_work(&library.id))
        .filter(|library| {
            if is_admin {
                library.admin_visible
            } else {
                library.access_mode == "everyone" || library.member_ids.contains(&user_id)
            }
        })
        .map(|library| library.id.clone())
        .collect();
    Ok(store
        .list_libraries()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|library| allowed.contains(&library.id))
        .collect())
}

fn folder_root_in(parent: &str, libraries: &[crate::store::Library]) -> Option<PathBuf> {
    libraries.iter().find_map(|library| {
        library
            .root_paths
            .iter()
            .find_map(|root| (folder_id_for_root(root) == parent).then(|| root.clone()))
    })
}

fn series_media_id(
    store: &crate::Store,
    user_id: UserId,
    id: &str,
    ownership: &store::LibraryOwnership,
    is_admin: bool,
) -> Result<(Option<MediaId>, Option<Vec<FileCoverage>>), String> {
    let Some(media_id) = protocol_series_media_id(id) else {
        return Ok((None, None));
    };
    let Some(media) = store
        .get_media(media_id)
        .map_err(|error| error.to_string())?
    else {
        return Ok((None, None));
    };
    if media.kind != MediaKind::Tv {
        return Ok((None, None));
    }
    let coverages = store
        .file_coverage_for_media(media.id)
        .map_err(|error| error.to_string())?;
    let visible = coverages.iter().any(|coverage| {
        let Some(owner) = ownership.library(&coverage.file.library_id.to_string()) else {
            return false;
        };
        ownership.accepts_work(&owner.id)
            && if is_admin {
                owner.admin_visible
            } else {
                owner.access_mode == "everyone" || owner.member_ids.contains(&user_id)
            }
    });
    // 可见性拒绝只丢掉分类身份，不丢掉已经读出的行：同一媒体稍后还会当显式目标用。
    Ok((visible.then_some(media.id), Some(coverages)))
}

pub(super) fn in_scope_folder(scope: &ScopedRead, coverage: &FileCoverage) -> bool {
    scope
        .folder_root
        .as_ref()
        .is_none_or(|root| coverage.file.path.starts_with(root))
}

pub(super) fn coverage_visible(scope: &ScopedRead, coverage: &FileCoverage) -> bool {
    if !scope.media.contains_key(&coverage.file.media_id) {
        return false;
    }
    let Some(owner) = scope
        .ownership
        .library(&coverage.file.library_id.to_string())
    else {
        return false;
    };
    scope.ownership.accepts_work(&owner.id)
        && scope
            .requested_library
            .as_ref()
            .is_none_or(|library_id| &owner.id == library_id)
        && if scope.is_admin {
            owner.admin_visible
        } else {
            owner.access_mode == "everyone" || owner.member_ids.contains(&scope.user_id)
        }
}

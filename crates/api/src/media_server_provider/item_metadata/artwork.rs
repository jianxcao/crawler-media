use domain::{LibraryFile, Media, MediaKind};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::PathBuf,
    time::UNIX_EPOCH,
};

fn remote_episode_thumb(thumb: &str) -> Option<String> {
    if thumb.starts_with("https://image.tmdb.org/") {
        Some(thumb.to_string())
    } else if thumb.starts_with("/t/p/") {
        Some(format!("https://image.tmdb.org{thumb}"))
    } else {
        None
    }
}

fn sidecar_episode_thumb(file: &LibraryFile) -> Option<PathBuf> {
    let path = file.path.as_path();
    let dir = path.parent()?;
    let stem = path.file_stem()?.to_str()?;
    ["-thumb.jpg", "-thumb.png", ".jpg", ".png"]
        .into_iter()
        .map(|suffix| dir.join(format!("{stem}{suffix}")))
        .find(|candidate| candidate.is_file())
}

pub fn backdrop_path(file: &LibraryFile, is_series: bool) -> Option<PathBuf> {
    let file_dir = file.path.parent()?;
    let parent_dir = file_dir.parent().unwrap_or(file_dir);
    let dirs = if is_series {
        [parent_dir, file_dir]
    } else {
        [file_dir, parent_dir]
    };
    dirs.into_iter()
        .flat_map(|dir| ["fanart.jpg", "backdrop.jpg"].map(|name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// 页内主图事实：只取「已采纳的 `still_url`」与文件系统旁车两处来源。
///
/// 这里刻意不读现场 NFO（`library::read_nfo`）：页内渲染的是已落库口径，sidecar 被改坏或
/// 删除都不得改变同一页的响应。图片端点（[`primary_image_url`] / [`local_primary_image`]）
/// 与页内同源：远端一律来自调用方传入的已采纳 `still_url`，本地一律来自文件系统约定。
pub(super) fn primary_image_metadata(
    file: &LibraryFile,
    unit: (i32, i32),
    media: &Media,
    is_series: bool,
    adopted_still_url: Option<&str>,
) -> (bool, Option<String>) {
    let episode = !is_series && media.kind == MediaKind::Tv;
    let local = episode.then(|| sidecar_episode_thumb(file)).flatten();
    let local_still = (!is_series && media.kind == MediaKind::Tv && unit.1 >= 0)
        .then(|| crate::episode_still::existing(file.path.as_path()))
        .flatten();
    let remote = episode
        .then(|| primary_image_url(unit, media, adopted_still_url))
        .flatten();
    let has_poster = crate::http::library::poster_path(file).is_some();
    if episode {
        tracing::debug!(
            path = %file.path.display(),
            adopted_still = adopted_still_url.is_some(),
            local = local.is_some(),
            local_still = local_still.is_some(),
            remote = remote.is_some(),
            poster = has_poster,
            "页内主图事实来自已采纳字段与文件系统"
        );
    }
    (
        local.is_some() || local_still.is_some() || remote.is_some() || has_poster,
        remote,
    )
}

/// 分集远端主图：只认调用方给的**已采纳** `still_url`，不读现场 NFO。
///
/// 页内 [`primary_image_metadata`] 与本函数共用同一入口，所以「页内声明有主图」与
/// 「端点能返回这张图」是同一个事实。
pub fn primary_image_url(
    unit: (i32, i32),
    media: &Media,
    adopted_still_url: Option<&str>,
) -> Option<String> {
    if media.kind != MediaKind::Tv || unit.1 < 0 {
        return None;
    }
    adopted_still_url.and_then(remote_episode_thumb)
}

/// 分集本地主图：文件系统约定（剧照旁车、海报）与已采纳字段无关，故无新增入参。
pub fn local_primary_image(file: &LibraryFile, unit: (i32, i32), media: &Media) -> Option<PathBuf> {
    let episode_still = (media.kind == MediaKind::Tv && unit.1 >= 0)
        .then(|| crate::episode_still::existing(file.path.as_path()))
        .flatten();
    episode_still.or_else(|| crate::http::library::poster_path(file))
}

pub fn series_primary_image_tag(
    file: &LibraryFile,
    media: &Media,
    is_series: bool,
) -> Option<String> {
    if !is_series || media.kind != MediaKind::Tv {
        return None;
    }
    let path = crate::http::library::poster_path(file)?;
    let metadata = std::fs::metadata(&path).ok()?;
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    metadata.len().hash(&mut hasher);
    if let Ok(modified) = metadata.modified() {
        modified
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .hash(&mut hasher);
    }
    Some(format!("poster-{:016x}", hasher.finish()))
}

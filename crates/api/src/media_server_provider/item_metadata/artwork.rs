use super::nfo::row_nfo_path;
use domain::{Media, MediaKind};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
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

fn local_episode_thumb(row: &domain::LedgerRow, thumb: Option<&str>) -> Option<PathBuf> {
    let path = Path::new(&row.path);
    let dir = path.parent()?;
    let stem = path.file_stem()?.to_str()?;
    if let Some(thumb) = thumb {
        let nfo = dir.join(format!("{stem}.nfo"));
        if Path::new(thumb).is_absolute() && Path::new(thumb).is_file() {
            return Some(PathBuf::from(thumb));
        }
        let candidate = nfo.parent()?.join(thumb);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    ["-thumb.jpg", "-thumb.png", ".jpg", ".png"]
        .into_iter()
        .map(|suffix| dir.join(format!("{stem}{suffix}")))
        .find(|candidate| candidate.is_file())
}

pub fn backdrop_path(row: &domain::LedgerRow, is_series: bool) -> Option<PathBuf> {
    let file_dir = Path::new(&row.path).parent()?;
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

pub(super) fn primary_image_metadata(
    row: &domain::LedgerRow,
    media: &Media,
    is_series: bool,
) -> (bool, Option<String>) {
    let nfo = row_nfo_path(row, media, is_series).and_then(|path| library::read_nfo(&path));
    let thumb = nfo.as_ref().and_then(|metadata| metadata.thumb.as_deref());
    let local = (!is_series && media.kind == MediaKind::Tv)
        .then(|| local_episode_thumb(row, thumb))
        .flatten();
    let local_still = (!is_series && media.kind == MediaKind::Tv && row.episode.is_some())
        .then(|| crate::episode_still::existing(Path::new(&row.path)))
        .flatten();
    let remote = (!is_series && media.kind == MediaKind::Tv)
        .then(|| thumb.and_then(remote_episode_thumb))
        .flatten();
    (
        local.is_some()
            || local_still.is_some()
            || remote.is_some()
            || crate::http::library::poster_path(row).is_some(),
        remote,
    )
}

pub fn primary_image_url(row: &domain::LedgerRow, media: &Media) -> Option<String> {
    if media.kind != MediaKind::Tv || row.episode.is_none() {
        return None;
    }
    row_nfo_path(row, media, false)
        .and_then(|path| library::read_nfo(&path))
        .and_then(|metadata| metadata.thumb)
        .and_then(|thumb| remote_episode_thumb(&thumb))
}

pub fn local_primary_image(row: &domain::LedgerRow, media: &Media) -> Option<PathBuf> {
    let episode_still = (media.kind == MediaKind::Tv && row.episode.is_some())
        .then(|| crate::episode_still::existing(Path::new(&row.path)))
        .flatten();
    let nfo_thumb = if media.kind == MediaKind::Tv && row.episode.is_some() {
        row_nfo_path(row, media, false)
            .and_then(|path| library::read_nfo(&path))
            .and_then(|metadata| metadata.thumb)
    } else {
        None
    };
    episode_still
        .or_else(|| {
            nfo_thumb
                .as_deref()
                .and_then(|thumb| local_episode_thumb(row, Some(thumb)))
        })
        .or_else(|| crate::http::library::poster_path(row))
}

pub fn series_primary_image_tag(
    row: &domain::LedgerRow,
    media: &Media,
    is_series: bool,
) -> Option<String> {
    if !is_series || media.kind != MediaKind::Tv {
        return None;
    }
    let path = crate::http::library::poster_path(row)?;
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

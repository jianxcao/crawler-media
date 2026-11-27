use std::path::{Path, PathBuf};

use downloader::Downloader;
use filter::ScoredTorrent;

use super::transfer::dest_path;

#[allow(clippy::too_many_arguments)]
pub fn plan_torrent_destinations<D: Downloader + ?Sized>(
    subscribe: &domain::Subscribe,
    media: &domain::Media,
    library_root: &Path,
    naming: Option<&str>,
    transfer_mode: Option<library::TransferMode>,
    absolute: Option<&crate::AbsoluteEpisodes>,
    downloader: &D,
    scored: &ScoredTorrent,
    existing: &[crate::collection_destinations::DestinationMapping],
) -> Vec<(PathBuf, PathBuf)> {
    let Ok((files, done)) = downloader.collectable_files(&scored.torrent) else {
        return Vec::new();
    };
    if !done
        && matches!(transfer_mode, Some(library::TransferMode::Move))
        && !downloader.is_claimed(&scored.torrent).unwrap_or(true)
    {
        return Vec::new();
    }
    let videos: Vec<PathBuf> = files
        .into_iter()
        .filter(|path| crate::sidecars::is_video(path))
        .collect();
    let single_file = videos.len() == 1;
    let mut planned = Vec::new();
    for src in videos {
        let source_path = src.display().to_string();
        if existing.iter().any(|m| m.source_path == source_path) {
            continue;
        }
        let file_release = release::parse(
            src.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
        );
        let (file_release, torrent_release) = match absolute {
            Some(map) if !release::has_explicit_season(&scored.torrent.title) => {
                (map.apply(&file_release), map.apply(&scored.release))
            }
            _ => (file_release, scored.release.clone()),
        };
        let Some(release) = crate::file_identity::resolve_file_release(
            subscribe,
            media,
            &src,
            &file_release,
            &torrent_release,
            single_file,
        ) else {
            continue;
        };
        if let Ok(dest) = dest_path(library_root, media, &release, &src, naming) {
            planned.push((src, dest));
        }
    }
    planned
}

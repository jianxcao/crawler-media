//! Remove the on-disk leftovers of a deleted media file: its own sidecars
//! (`<stem>.nfo`, `<stem>-thumb.jpg`, subtitles…) and, once a directory holds
//! nothing but sidecars, the directory-level artwork/NFO and the directory
//! itself, walking up to (never including) the Library root.
//!
//! Only known sidecar extensions are ever deleted; a directory containing any
//! other file or any subdirectory is left untouched.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::is_video_file;

const SIDECAR_EXT: &[&str] = &[
    "nfo", "jpg", "jpeg", "png", "webp", "gif", "tbn", "bif", "srt", "ass", "ssa", "sub", "idx",
    "sup", "vtt",
];
const JUNK_NAMES: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];

fn is_sidecar(path: &Path) -> bool {
    if path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| JUNK_NAMES.contains(&n))
    {
        return true;
    }
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SIDECAR_EXT.iter().any(|k| e.eq_ignore_ascii_case(k)))
}

fn remove(path: &Path, removed: &mut Vec<PathBuf>) {
    match std::fs::remove_file(path) {
        Ok(()) => removed.push(path.to_path_buf()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(path = %path.display(), %error, "删除附属文件失败"),
    }
}

enum ReadDirOutcome {
    Entries(Vec<PathBuf>),
    Missing,
    Failed,
}

fn read_entries(dir: &Path) -> ReadDirOutcome {
    let entries = std::fs::read_dir(dir).and_then(|entries| {
        entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    });
    match entries {
        Ok(entries) => ReadDirOutcome::Entries(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path = %dir.display(), "附属文件目录已不存在，跳过清理");
            ReadDirOutcome::Missing
        }
        Err(error) => {
            tracing::warn!(path = %dir.display(), %error, "读取附属文件目录失败，停止清理");
            ReadDirOutcome::Failed
        }
    }
}

fn matches_stem(name: &OsStr, stem: &OsStr) -> bool {
    name.as_encoded_bytes()
        .strip_prefix(stem.as_encoded_bytes())
        .is_some_and(|suffix| matches!(suffix.first(), Some(b'.' | b'-')))
}

fn remove_matching_stem_sidecars(entries: &[PathBuf], stem: &OsStr, removed: &mut Vec<PathBuf>) {
    let remaining_stems: Vec<&OsStr> = entries
        .iter()
        .filter(|path| is_video_file(path))
        .filter_map(|path| path.file_stem())
        .collect();
    for path in entries {
        let Some(name) = path.file_name() else {
            continue;
        };
        // The longest matching video stem owns a sidecar. Equal stems share it.
        let shared = remaining_stems
            .iter()
            .any(|other| other.len() >= stem.len() && matches_stem(name, other));
        if path.is_file() && is_sidecar(path) && matches_stem(name, stem) && !shared {
            remove(path, removed);
        }
    }
}

/// Clean up after `deleted` (already removed) inside Library `root`.
/// Only media files may trigger adjacent sidecar/directory pruning.
/// Returns every path removed (files and directories).
pub fn prune_after_delete(deleted: &Path, root: &Path) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    if is_sidecar(deleted) {
        return removed;
    }
    let (Some(dir), Some(stem)) = (deleted.parent(), deleted.file_stem()) else {
        return removed;
    };
    if !dir.starts_with(root) {
        return removed;
    }
    let mut current = match read_entries(dir) {
        ReadDirOutcome::Failed => return removed,
        ReadDirOutcome::Entries(entries) => {
            remove_matching_stem_sidecars(&entries, stem, &mut removed);
            dir.to_path_buf()
        }
        ReadDirOutcome::Missing => {
            let Some(parent) = dir.parent() else {
                return removed;
            };
            parent.to_path_buf()
        }
    };
    while current != root && current.starts_with(root) {
        match read_entries(&current) {
            ReadDirOutcome::Failed => break,
            ReadDirOutcome::Missing => {}
            ReadDirOutcome::Entries(entries) => {
                if entries.iter().any(|p| !p.is_file() || !is_sidecar(p)) {
                    break;
                }
                for path in &entries {
                    remove(path, &mut removed);
                }
                match std::fs::remove_dir(&current) {
                    Ok(()) => removed.push(current.clone()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        tracing::debug!(path = %current.display(), "附属空目录已不存在，继续向上清理");
                    }
                    Err(error) => {
                        tracing::warn!(path = %current.display(), %error, "删除空目录失败");
                        break;
                    }
                }
            }
        }
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent.to_path_buf();
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    #[test]
    fn episode_delete_keeps_siblings_and_show_artwork() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("tv");
        let season = root.join("Show (2020)/Season 01");
        let e1 = season.join("Show S01E01.strm");
        let e2 = season.join("Show S01E02.strm");
        for p in [&e1, &e2] {
            touch(p);
        }
        touch(&season.join("Show S01E01.nfo"));
        touch(&season.join("Show S01E01-thumb.jpg"));
        touch(&season.join("Show S01E01.zh.srt"));
        touch(&season.join("Show S01E02.nfo"));
        touch(&season.join("season.nfo"));
        touch(&root.join("Show (2020)/tvshow.nfo"));
        fs::remove_file(&e1).unwrap();
        prune_after_delete(&e1, &root);
        assert!(!season.join("Show S01E01.nfo").exists());
        assert!(!season.join("Show S01E01-thumb.jpg").exists());
        assert!(!season.join("Show S01E01.zh.srt").exists());
        assert!(season.join("Show S01E02.nfo").exists());
        assert!(season.join("season.nfo").exists());
        assert!(root.join("Show (2020)/tvshow.nfo").exists());
    }

    #[test]
    fn last_episode_removes_season_and_show_dirs_but_not_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("tv");
        let show = root.join("Show (2020)");
        let season = show.join("Season 01");
        let e1 = season.join("Show S01E01.strm");
        touch(&e1);
        touch(&season.join("Show S01E01.nfo"));
        touch(&season.join("season.nfo"));
        touch(&show.join("tvshow.nfo"));
        touch(&show.join("poster.jpg"));
        touch(&show.join("logo.png"));
        touch(&root.join("Other/Other S01E01.strm"));
        fs::remove_file(&e1).unwrap();
        prune_after_delete(&e1, &root);
        assert!(!show.exists());
        assert!(root.join("Other/Other S01E01.strm").exists());
        assert!(root.exists());
    }

    #[test]
    fn movie_dir_with_foreign_file_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("movie");
        let dir = root.join("Film (2023)");
        let film = dir.join("Film (2023).strm");
        touch(&film);
        touch(&dir.join("Film (2023).nfo"));
        touch(&dir.join("fanart.jpg"));
        touch(&dir.join("notes.txt"));
        fs::remove_file(&film).unwrap();
        prune_after_delete(&film, &root);
        assert!(!dir.join("Film (2023).nfo").exists());
        assert!(dir.join("fanart.jpg").exists());
        assert!(dir.join("notes.txt").exists());
    }

    #[test]
    fn movie_dir_with_only_artwork_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("movie");
        let dir = root.join("Film (2023)");
        let film = dir.join("Film (2023).strm");
        touch(&film);
        for n in [
            "Film (2023).nfo",
            "fanart.jpg",
            "poster.jpg",
            "clearlogo.png",
            "banner.jpg",
        ] {
            touch(&dir.join(n));
        }
        fs::remove_file(&film).unwrap();
        prune_after_delete(&film, &root);
        assert!(!dir.exists());
        assert!(root.exists());
    }

    struct WarnCounter(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl tracing::Subscriber for WarnCounter {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::WARN
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() <= tracing::Level::WARN {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn missing_parent_dir_skips_cleanup_without_warn_log() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("tv");
        fs::create_dir_all(&root).unwrap();
        let already_moved = root.join("两京十五日 (2026)/Season 01/两京十五日 S01E01.mkv");
        let warns = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let removed = tracing::subscriber::with_default(WarnCounter(warns.clone()), || {
            prune_after_delete(&already_moved, &root)
        });
        assert!(removed.is_empty());
        assert_eq!(
            warns.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "already-moved directory during missing-row cleanup should not emit WARN"
        );
    }

    #[test]
    fn missing_season_dir_still_prunes_sidecar_only_show_dir_without_warn() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("tv");
        let show = root.join("Show (2020)");
        touch(&show.join("tvshow.nfo"));
        touch(&show.join("poster.jpg"));
        let missing_episode = show.join("Season 01/Show S01E01.mkv");

        let warns = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let removed = tracing::subscriber::with_default(WarnCounter(warns.clone()), || {
            prune_after_delete(&missing_episode, &root)
        });

        assert!(
            !show.exists(),
            "Show (2020) holding only tvshow.nfo + poster.jpg must be pruned even when Season 01 was already missing; removed={removed:?}"
        );
        assert!(root.exists());
        assert_eq!(
            warns.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "pruning leftover show dir when season dir is missing must not emit WARN"
        );
    }
}

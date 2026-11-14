//! Remove the on-disk leftovers of a deleted media file: its own sidecars
//! (`<stem>.nfo`, `<stem>-thumb.jpg`, subtitles…) and, once a directory holds
//! nothing but sidecars, the directory-level artwork/NFO and the directory
//! itself, walking up to (never including) the Library root.
//!
//! Only known sidecar extensions are ever deleted; a directory containing any
//! other file or any subdirectory is left untouched.

use std::path::{Path, PathBuf};

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

/// Clean up after `deleted` (already removed) inside Library `root`.
/// Returns every path removed (files and directories).
pub fn prune_after_delete(deleted: &Path, root: &Path) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let (Some(dir), Some(stem)) = (
        deleted.parent(),
        deleted.file_stem().and_then(|s| s.to_str()),
    ) else {
        return removed;
    };
    if !dir.starts_with(root) {
        return removed;
    }
    let own_prefixes = [format!("{stem}."), format!("{stem}-")];
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_file()
                && is_sidecar(&path)
                && own_prefixes.iter().any(|p| name.starts_with(p.as_str()))
            {
                remove(&path, &mut removed);
            }
        }
    }
    let mut current = dir.to_path_buf();
    while current != root && current.starts_with(root) {
        let Ok(entries) = std::fs::read_dir(&current) else {
            break;
        };
        let entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        if entries.iter().any(|p| !p.is_file() || !is_sidecar(p)) {
            break;
        }
        for path in &entries {
            remove(path, &mut removed);
        }
        match std::fs::remove_dir(&current) {
            Ok(()) => removed.push(current.clone()),
            Err(error) => {
                tracing::warn!(path = %current.display(), %error, "删除空目录失败");
                break;
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
}

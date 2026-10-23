//! Risk 4: directory-level artwork (`poster.jpg`, `fanart.jpg`, …) in a rename /
//! re-parent intent. Ownership is decided in the `begin` transaction: a sidecar that
//! no other recorded file can be using moves with the video; one that another file
//! in the same directory (or an artwork/scrape reference) still uses is copied to the
//! destination and kept at the source. The disk phase copies through a temp name and
//! an atomic rename, so a crash never leaves a torn destination file.
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use super::rename_durable::RenameSidecar;
use crate::StoreError;

/// How a sidecar reaches the destination. Persisted in `sidecars_json` (required).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidecarMode {
    Move,
    Copy,
}

/// Artwork kinds media servers read from a video's folder (`poster.jpg`) or as a
/// prefixed variant (`season01-poster.jpg`, `show-fanart.png`, `<stem>-thumb.jpg`).
const ARTWORK_KINDS: &[&str] = &[
    "poster",
    "folder",
    "cover",
    "fanart",
    "backdrop",
    "banner",
    "landscape",
    "thumb",
    "clearlogo",
    "logo",
    "clearart",
    "disc",
    "discart",
    "keyart",
];
const ARTWORK_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "tbn"];

fn is_artwork_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some((stem, ext)) = lower.rsplit_once('.') else {
        return false;
    };
    if !ARTWORK_EXTS.contains(&ext) {
        return false;
    }
    ARTWORK_KINDS.iter().any(|kind| {
        stem == *kind
            || stem
                .strip_suffix(kind)
                .is_some_and(|prefix| prefix.ends_with('-') && prefix.len() > 1)
    })
}

/// Lock-free observation (one `read_dir`): artwork beside `from` when the video
/// changes folder. Names prefixed with the video stem are renamed to the new stem.
pub(crate) fn observe_directory_artwork(from: &Path, to: &Path, out: &mut Vec<RenameSidecar>) {
    let (Some(from_dir), Some(to_dir)) = (from.parent(), to.parent()) else {
        return;
    };
    if from_dir == to_dir {
        return;
    }
    let from_stem = from.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let to_stem = to.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let Ok(entries) = std::fs::read_dir(from_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let source = from_dir.join(&name);
        if !is_artwork_name(&name) || !source.is_file() || out.iter().any(|s| s.from == source) {
            continue;
        }
        let target_name = match name.strip_prefix(from_stem) {
            Some(rest) if !from_stem.is_empty() => format!("{to_stem}{rest}"),
            _ => name.clone(),
        };
        out.push(RenameSidecar {
            kind: "artwork".into(),
            from: source,
            to: to_dir.join(target_name),
            mode: SidecarMode::Move,
        });
    }
}

/// Execution-time re-check, run under the Store lock immediately before sidecar
/// bytes move: sharing is classified again (a file published after `begin` makes a
/// planned move a copy), and the destination-exists policy is applied:
/// - destination absent: move (sole owner) or copy (shared);
/// - destination present with identical bytes: nothing to write; a sole-owned
///   source counts as moved and is queued for deletion at completion;
/// - destination present with different bytes: the destination wins and the source
///   is kept (never overwritten, never deleted).
pub(crate) fn recheck_for_execution(
    tx: &Connection,
    file_id: &str,
    from_video: &Path,
    sidecars: &mut [RenameSidecar],
) -> Result<(), StoreError> {
    classify_shared_sidecars(tx, file_id, from_video, sidecars)?;
    for side in sidecars.iter_mut() {
        if side.mode == SidecarMode::Move
            && side.from.is_file()
            && side.to.is_file()
            && !same_bytes(&side.from, &side.to)
        {
            tracing::warn!(from = %side.from.display(), to = %side.to.display(),
                "destination artwork differs; keeping both, destination wins");
            side.mode = SidecarMode::Copy;
        }
    }
    Ok(())
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    ma.len() == mb.len() && std::fs::read(a).ok() == std::fs::read(b).ok()
}

/// In the `begin` transaction: directory-level sidecars still used by another
/// recorded file become `Copy`. Same-stem sidecars belong to this video alone.
pub(crate) fn classify_shared_sidecars(
    tx: &Connection,
    file_id: &str,
    from_video: &Path,
    sidecars: &mut [RenameSidecar],
) -> Result<(), StoreError> {
    let Some(dir) = from_video.parent() else {
        return Ok(());
    };
    let stem = from_video
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let siblings = sibling_files(tx, file_id, dir)?;
    for side in sidecars.iter_mut() {
        let own_stem = side
            .from
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| !stem.is_empty() && n.starts_with(stem));
        if own_stem || side.from.parent() != Some(dir) {
            continue;
        }
        if siblings || referenced_elsewhere(tx, file_id, &side.from)? {
            side.mode = SidecarMode::Copy;
            tracing::info!(file_id, sidecar = %side.from.display(), "shared sidecar will be copied, source kept");
        }
    }
    Ok(())
}

fn sibling_files(tx: &Connection, file_id: &str, dir: &Path) -> Result<bool, StoreError> {
    let prefix = format!(
        "{}/",
        crate::path_key::normalize_path_key(&dir.to_string_lossy()).trim_end_matches('/')
    );
    // `'/' + 1 == '0'`: the half-open range covers every key under `prefix`.
    let upper = format!("{}0", prefix.trim_end_matches('/'));
    let mut stmt = tx.prepare(
        "SELECT path_key FROM library_files WHERE file_id <> ?1 AND path_key >= ?2 AND path_key < ?3",
    )?;
    let keys = stmt
        .query_map(params![file_id, prefix, upper], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let dir_key = PathBuf::from(prefix.trim_end_matches('/'));
    Ok(keys
        .iter()
        .any(|k| Path::new(k).parent() == Some(dir_key.as_path())))
}

fn referenced_elsewhere(tx: &Connection, file_id: &str, path: &Path) -> Result<bool, StoreError> {
    let path = path.to_string_lossy();
    let used: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_files WHERE file_id <> ?1
                        AND (poster_path = ?2 OR backdrop_path = ?2 OR still_path = ?2))
             OR EXISTS(SELECT 1 FROM scrape_sidecars WHERE file_id <> ?1 AND path = ?2)",
        params![file_id, path],
        |r| r.get(0),
    )?;
    Ok(used)
}

/// Disk phase for one sidecar. Idempotent for crash recovery.
pub(crate) fn apply_sidecar(side: &RenameSidecar) -> Result<(), StoreError> {
    if !side.from.is_file() || side.to.exists() {
        return Ok(());
    }
    if let Some(parent) = side.to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match side.mode {
        SidecarMode::Move => match std::fs::rename(&side.from, &side.to) {
            // Another filesystem: copy; the left-behind source is queued for
            // deletion once the rename is acknowledged.
            Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => copy_sidecar(side)?,
            other => other?,
        },
        SidecarMode::Copy => copy_sidecar(side)?,
    }
    Ok(())
}

fn copy_sidecar(side: &RenameSidecar) -> Result<(), StoreError> {
    let name = side
        .to
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("sidecar");
    let temp = side.to.with_file_name(format!(".{name}.partial.tmp"));
    std::fs::copy(&side.from, &temp)?;
    std::fs::rename(&temp, &side.to)?;
    Ok(())
}

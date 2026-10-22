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

/// Directory artwork names media servers read from a video's folder.
const DIRECTORY_ARTWORK: &[&str] = &[
    "poster.jpg",
    "poster.png",
    "folder.jpg",
    "folder.png",
    "cover.jpg",
    "fanart.jpg",
    "fanart.png",
    "backdrop.jpg",
    "banner.jpg",
    "landscape.jpg",
    "thumb.jpg",
    "clearlogo.png",
    "logo.png",
    "clearart.png",
    "disc.png",
];

/// Lock-free observation: directory artwork beside `from` when the video changes folder.
pub(crate) fn observe_directory_artwork(from: &Path, to: &Path, out: &mut Vec<RenameSidecar>) {
    let (Some(from_dir), Some(to_dir)) = (from.parent(), to.parent()) else {
        return;
    };
    if from_dir == to_dir {
        return;
    }
    for name in DIRECTORY_ARTWORK {
        let source = from_dir.join(name);
        if source.is_file() && !out.iter().any(|s| s.from == source) {
            out.push(RenameSidecar {
                kind: "artwork".into(),
                from: source,
                to: to_dir.join(name),
                mode: SidecarMode::Move,
            });
        }
    }
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
        SidecarMode::Move => std::fs::rename(&side.from, &side.to)?,
        SidecarMode::Copy => {
            let name = side
                .to
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("sidecar");
            let temp = side.to.with_file_name(format!(".{name}.partial.tmp"));
            std::fs::copy(&side.from, &temp)?;
            std::fs::rename(&temp, &side.to)?;
        }
    }
    Ok(())
}

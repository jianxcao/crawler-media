//! Risk 4: directory-level artwork (`poster.jpg`, `fanart.jpg`, …) in a rename /
//! re-parent intent. A first ownership guess is made in `begin`; the binding
//! decision ([`SidecarAction`]) is made when the disk receipt is recorded: bytes are
//! compared outside every lock ([`observe_targets`]), sharing is re-checked in a
//! short transaction ([`decide_for_execution`]) and the decision is persisted with
//! the intent. Publication, references and source deletion read only the persisted
//! decision. Destinations are written no-clobber (hard link), so a competitor that
//! appears after observation is never overwritten.
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use super::rename_durable::RenameSidecar;
use crate::StoreError;

/// Durable per-sidecar decision. Persisted in `sidecars_json` (required).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidecarAction {
    /// Sole owner, destination free: the bytes move; the source is deleted.
    Move,
    /// Shared with another file: copied, source kept and still referenced by others.
    Copy,
    /// Destination holds different bytes: destination wins, the source is kept and
    /// references stay on the retained source.
    KeepSource,
    /// Destination already holds identical bytes and this file is the sole owner:
    /// nothing is written; the source is queued for deletion at completion.
    AlreadyIdentical,
}

impl SidecarAction {
    /// References follow the sidecar to its destination.
    pub(crate) fn migrates_references(self) -> bool {
        !matches!(self, Self::KeepSource)
    }

    /// The source bytes are left over and owned by nobody else once published.
    pub(crate) fn deletes_source(self) -> bool {
        matches!(self, Self::Move | Self::AlreadyIdentical)
    }
}

/// Lock-free observation of one sidecar destination against its source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TargetComparison {
    /// The source is gone (already moved, or never present after a crash).
    SourceAbsent,
    DestinationAbsent,
    Identical,
    Different,
    /// Either side could not be read: never treated as identical.
    Unknown(String),
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
            action: SidecarAction::Move,
        });
    }
}

/// Outside every lock: compare each sidecar's destination with its source by a
/// bounded streaming read (no whole-file buffers).
pub(crate) fn observe_targets(sidecars: &[RenameSidecar]) -> Vec<TargetComparison> {
    sidecars
        .iter()
        .map(|side| compare_target(&side.from, &side.to))
        .collect()
}

fn compare_target(from: &Path, to: &Path) -> TargetComparison {
    match std::fs::symlink_metadata(from) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return TargetComparison::SourceAbsent;
        }
        Err(error) => return TargetComparison::Unknown(format!("{}: {error}", from.display())),
        Ok(meta) if !meta.is_file() => {
            return TargetComparison::Unknown(format!("{} is not a regular file", from.display()));
        }
        Ok(_) => {}
    }
    match std::fs::symlink_metadata(to) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            TargetComparison::DestinationAbsent
        }
        Err(error) => TargetComparison::Unknown(format!("{}: {error}", to.display())),
        Ok(meta) if !meta.is_file() => {
            TargetComparison::Unknown(format!("{} is not a regular file", to.display()))
        }
        Ok(_) => match same_bytes(from, to) {
            Ok(true) => TargetComparison::Identical,
            Ok(false) => TargetComparison::Different,
            Err(error) => TargetComparison::Unknown(error.to_string()),
        },
    }
}

/// Streaming byte equality; any read failure is an error, never "equal".
pub(crate) fn same_bytes(a: &Path, b: &Path) -> std::io::Result<bool> {
    let (mut fa, mut fb) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = read_full(&mut fa, &mut ba)?;
        let m = read_full(&mut fb, &mut bb)?;
        if n != m || ba[..n] != bb[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

fn read_full(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::io::Read;
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Short transaction, no IO: re-classify sharing (a file published after `begin`
/// turns a move into a copy) and bind each sidecar to its durable action from the
/// lock-free comparison. An unknown observation fails the record (retryable) and
/// keeps the last persisted decision.
pub(crate) fn decide_for_execution(
    tx: &Connection,
    file_id: &str,
    from_video: &Path,
    sidecars: &mut [RenameSidecar],
    observed: &[TargetComparison],
) -> Result<(), StoreError> {
    for side in sidecars.iter_mut() {
        // Re-derive sharing from scratch, never from a stale persisted decision.
        side.action = SidecarAction::Move;
    }
    classify_shared_sidecars(tx, file_id, from_video, sidecars)?;
    for (side, seen) in sidecars.iter_mut().zip(observed) {
        let shared = side.action == SidecarAction::Copy;
        side.action = match seen {
            TargetComparison::SourceAbsent | TargetComparison::DestinationAbsent => side.action,
            TargetComparison::Identical if shared => SidecarAction::Copy,
            TargetComparison::Identical => SidecarAction::AlreadyIdentical,
            TargetComparison::Different => {
                tracing::warn!(from = %side.from.display(), to = %side.to.display(),
                    "destination sidecar differs; destination wins, source kept");
                SidecarAction::KeepSource
            }
            TargetComparison::Unknown(error) => {
                return Err(StoreError::Io(std::io::Error::other(format!(
                    "sidecar {} -> {}: {error}",
                    side.from.display(),
                    side.to.display()
                ))));
            }
        };
    }
    Ok(())
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
            side.action = SidecarAction::Copy;
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

/// Disk phase for one sidecar, outside the Store lock. Idempotent for crash
/// recovery. The destination is created no-clobber; a competitor that appeared
/// after observation turns the decision into `KeepSource` (returned `true`).
pub(crate) fn apply_sidecar(side: &mut RenameSidecar) -> Result<bool, StoreError> {
    if matches!(
        side.action,
        SidecarAction::KeepSource | SidecarAction::AlreadyIdentical
    ) || !side.from.is_file()
    {
        return Ok(false);
    }
    if let Some(parent) = side.to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let placed = match side.action {
        SidecarAction::Move => match std::fs::hard_link(&side.from, &side.to) {
            // Another filesystem: copy; the source is deleted at completion.
            Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => copy_sidecar(side),
            Ok(()) => std::fs::remove_file(&side.from).map_err(Into::into),
            Err(error) => Err(error.into()),
        },
        _ => copy_sidecar(side),
    };
    match placed {
        Ok(()) => Ok(false),
        Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if same_bytes(&side.from, &side.to).unwrap_or(false) {
                return Ok(false);
            }
            tracing::warn!(from = %side.from.display(), to = %side.to.display(),
                "destination sidecar appeared after observation; not overwritten, source kept");
            side.action = SidecarAction::KeepSource;
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

fn copy_sidecar(side: &RenameSidecar) -> Result<(), StoreError> {
    let name = side
        .to
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("sidecar");
    let temp = side.to.with_file_name(format!(".{name}.partial.tmp"));
    std::fs::copy(&side.from, &temp)?;
    // No-clobber publish: link fails with AlreadyExists instead of replacing.
    let linked = std::fs::hard_link(&temp, &side.to);
    let _ = std::fs::remove_file(&temp);
    linked?;
    Ok(())
}

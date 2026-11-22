//! Risk 4: directory-level artwork (`poster.jpg`, `fanart.jpg`, …) in a rename /
//! re-parent intent. A first ownership guess is made in `begin`; the binding
//! decision ([`SidecarAction`]) is made when the disk receipt is recorded: bytes are
//! compared outside every lock ([`observe_targets`]), sharing is re-checked in a
//! short transaction ([`decide_for_execution`]) and the decision is persisted with
//! the intent. Publication, references and source deletion read only the persisted
//! decision. Destinations are written no-clobber (hard link), so a competitor that
//! appears after observation is never overwritten.
use std::collections::HashSet;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use super::rename_disk::mtime_ns;
use super::rename_durable::RenameSidecar;
use crate::StoreError;

/// Native stat identity of an original sidecar source file captured before any
/// disk cleanup. Leftover deletion only queues a file still matching this proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarSourceIdentity {
    pub size_bytes: u64,
    pub device: u64,
    pub inode: u64,
    pub mtime_ns: i64,
}

impl SidecarSourceIdentity {
    pub(crate) fn observe(path: &Path) -> Option<Self> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        Self::from_meta(&meta)
    }

    pub(crate) fn from_meta(meta: &Metadata) -> Option<Self> {
        meta.is_file().then(|| Self {
            size_bytes: meta.len(),
            device: meta.dev(),
            inode: meta.ino(),
            mtime_ns: mtime_ns(meta),
        })
    }

    pub(crate) fn matches(&self, meta: &Metadata) -> bool {
        meta.is_file()
            && meta.len() == self.size_bytes
            && meta.dev() == self.device
            && meta.ino() == self.inode
            && mtime_ns(meta) == self.mtime_ns
    }
}

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
    /// Source and destination resolve to the exact same physical directory entry
    /// (for example via a parent symlink alias or case-insensitive rename):
    /// nothing is written or unlinked, and the entry is never queued for deletion.
    SameEntry,
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

/// Lock-free observation of one sidecar destination against its source, plus
/// the native identity of the source and identical destination when present.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservedSidecarTarget {
    pub(crate) comparison: TargetComparison,
    pub(crate) source_identity: Option<SidecarSourceIdentity>,
    pub(crate) target_identity: Option<SidecarSourceIdentity>,
}

/// Lock-free observation of one sidecar destination against its source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TargetComparison {
    /// The source is gone (already moved, or never present after a crash).
    SourceAbsent,
    DestinationAbsent,
    SameEntry,
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

/// Returns the exact video or folder prefix, or an empty prefix for bare artwork names.
fn artwork_prefix(name: &str) -> Option<&str> {
    let (stem, ext) = name.rsplit_once('.')?;
    if !ARTWORK_EXTS
        .iter()
        .any(|known| ext.eq_ignore_ascii_case(known))
    {
        return None;
    }
    let (prefix, kind) = stem.rsplit_once('-').unwrap_or(("", stem));
    ARTWORK_KINDS
        .iter()
        .any(|known| kind.eq_ignore_ascii_case(known))
        .then_some(prefix)
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
    let mut video_stems = HashSet::new();
    let mut artwork = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if library::is_video_file(&path) {
            if let Some(stem) = path.file_stem() {
                video_stems.insert(stem.to_owned());
            }
        } else if let Some(name) = path.file_name().and_then(|name| name.to_str())
            && artwork_prefix(name).is_some()
            && path.is_file()
        {
            artwork.push(path);
        }
    }
    for source in artwork {
        let Some(name) = source.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let prefix = artwork_prefix(name).unwrap_or("");
        if out.iter().any(|s| s.from == source) {
            continue;
        }
        let target_name = if !prefix.is_empty() && prefix == from_stem {
            format!("{to_stem}{}", &name[prefix.len()..])
        } else if !prefix.is_empty() && video_stems.contains(std::ffi::OsStr::new(prefix)) {
            continue;
        } else {
            name.to_string()
        };
        let source_identity = SidecarSourceIdentity::observe(&source);
        out.push(RenameSidecar {
            kind: "artwork".into(),
            from: source,
            to: to_dir.join(target_name),
            action: SidecarAction::Move,
            source_identity,
            target_identity: None,
        });
    }
}

/// Outside every lock: compare each sidecar's destination with its source by a
/// bounded streaming read (no whole-file buffers).
pub(crate) fn observe_targets(sidecars: &[RenameSidecar]) -> Vec<ObservedSidecarTarget> {
    sidecars
        .iter()
        .map(|side| compare_target(&side.from, &side.to))
        .collect()
}

fn compare_target(from: &Path, to: &Path) -> ObservedSidecarTarget {
    let source_identity = match std::fs::symlink_metadata(from) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ObservedSidecarTarget {
                comparison: TargetComparison::SourceAbsent,
                source_identity: None,
                target_identity: None,
            };
        }
        Err(error) => {
            return ObservedSidecarTarget {
                comparison: TargetComparison::Unknown(format!("{}: {error}", from.display())),
                source_identity: None,
                target_identity: None,
            };
        }
        Ok(meta) if !meta.is_file() => {
            return ObservedSidecarTarget {
                comparison: TargetComparison::Unknown(format!(
                    "{} is not a regular file",
                    from.display()
                )),
                source_identity: None,
                target_identity: None,
            };
        }
        Ok(meta) => SidecarSourceIdentity::from_meta(&meta),
    };
    match super::rename_disk::same_directory_entry(from, to) {
        Ok(true) => {
            return ObservedSidecarTarget {
                comparison: TargetComparison::SameEntry,
                source_identity,
                target_identity: None,
            };
        }
        Ok(false) => {}
        Err(error) => {
            return ObservedSidecarTarget {
                comparison: TargetComparison::Unknown(format!("{}: {error}", to.display())),
                source_identity: None,
                target_identity: None,
            };
        }
    }
    let (comparison, target_identity) = match std::fs::symlink_metadata(to) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (TargetComparison::DestinationAbsent, None)
        }
        Err(error) => (
            TargetComparison::Unknown(format!("{}: {error}", to.display())),
            None,
        ),
        Ok(meta) if !meta.is_file() => (
            TargetComparison::Unknown(format!("{} is not a regular file", to.display())),
            None,
        ),
        Ok(to_meta) => match same_bytes(from, to) {
            Ok(true) => {
                let target_id = SidecarSourceIdentity::from_meta(&to_meta).filter(|id| {
                    std::fs::symlink_metadata(to)
                        .ok()
                        .is_some_and(|after| id.matches(&after))
                });
                if target_id.is_some() {
                    (TargetComparison::Identical, target_id)
                } else {
                    (
                        TargetComparison::Unknown(format!(
                            "{} changed during compare",
                            to.display()
                        )),
                        None,
                    )
                }
            }
            Ok(false) => (TargetComparison::Different, None),
            Err(error) => (TargetComparison::Unknown(error.to_string()), None),
        },
    };
    ObservedSidecarTarget {
        comparison,
        source_identity,
        target_identity,
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
    observed: &[ObservedSidecarTarget],
) -> Result<(), StoreError> {
    for side in sidecars.iter_mut() {
        // Re-derive sharing from scratch, never from a stale persisted decision.
        side.action = SidecarAction::Move;
    }
    classify_shared_sidecars(tx, file_id, from_video, sidecars)?;
    for (side, seen) in sidecars.iter_mut().zip(observed) {
        let shared = side.action == SidecarAction::Copy;
        side.action = match &seen.comparison {
            TargetComparison::SourceAbsent | TargetComparison::DestinationAbsent => side.action,
            TargetComparison::SameEntry => SidecarAction::SameEntry,
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
        side.source_identity = if side.action.deletes_source() {
            match seen.comparison {
                // If the source is already absent, do not invent fresh proof; only keep
                // previously persisted identity if any (never matches a newly created file).
                TargetComparison::SourceAbsent => side.source_identity.clone(),
                _ => seen.source_identity.clone(),
            }
        } else {
            None
        };
        side.target_identity = if side.action == SidecarAction::AlreadyIdentical {
            seen.target_identity.clone()
        } else if side.action.deletes_source()
            && matches!(seen.comparison, TargetComparison::SourceAbsent)
        {
            side.target_identity.clone()
        } else {
            None
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
    let siblings = sibling_files(tx, file_id, None, dir)?;
    let alternate_same_stem = alternate_video_same_stem(tx, file_id, None, dir, from_video)?;
    for side in sidecars.iter_mut() {
        let own_stem = side
            .from
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| {
                !stem.is_empty()
                    && (side.from.file_stem() == from_video.file_stem()
                        || artwork_prefix(name) == Some(stem))
            });
        if ((!own_stem || alternate_same_stem) && side.from.parent() == Some(dir) && siblings)
            || referenced_elsewhere(tx, file_id, None, &side.from)?
        {
            side.action = SidecarAction::Copy;
            tracing::info!(file_id, sidecar = %side.from.display(), "shared sidecar will be copied, source kept");
        }
    }
    Ok(())
}

/// Whether a same-entry NFO source in `from_library` is still needed by another
/// file in `from_library` (e.g., `tvshow.nfo` or an alternate same-stem cut).
pub(super) fn nfo_retained_in_library(
    tx: &Connection,
    from_library: &str,
    file_id: &str,
    from_video: &Path,
    from_nfo: &Path,
) -> Result<bool, StoreError> {
    let Some(dir) = from_video.parent() else {
        return Ok(false);
    };
    let own_stem = from_video
        .file_stem()
        .is_some_and(|stem| !stem.is_empty() && from_nfo.file_stem() == Some(stem));
    let siblings = sibling_files(tx, file_id, Some(from_library), dir)?;
    let same_stem = alternate_video_same_stem(tx, file_id, Some(from_library), dir, from_video)?;
    Ok((!own_stem && siblings)
        || (own_stem && same_stem)
        || referenced_elsewhere(tx, file_id, Some(from_library), from_nfo)?)
}

fn alternate_video_same_stem(
    tx: &Connection,
    file_id: &str,
    library_id: Option<&str>,
    dir: &Path,
    from_video: &Path,
) -> Result<bool, StoreError> {
    let dir_key = crate::path_key::canonical_recorded_key(tx, &dir.to_string_lossy());
    let current_key = crate::path_key::canonical_recorded_key(tx, &from_video.to_string_lossy());
    let Some(current_stem) = Path::new(&current_key).file_stem().and_then(|s| s.to_str()) else {
        return Ok(false);
    };
    let base = dir_key.trim_end_matches('/');
    // '.' (0x2e) .. '/' (0x2f) bounds every `<dir>/<stem>.<ext>` path in `dir`.
    let lower = format!("{base}/{current_stem}.");
    let upper = format!("{base}/{current_stem}/");
    let mut stmt = tx.prepare(
        "SELECT path_key FROM library_files
         WHERE parent_dir_key = ?2 AND path_key >= ?3 AND path_key < ?4 AND file_id <> ?1
           AND (?5 IS NULL OR library_id = ?5)",
    )?;
    let mut rows = stmt.query(params![file_id, dir_key, lower, upper, library_id])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let candidate = Path::new(&path);
        if candidate.file_stem() == Some(std::ffi::OsStr::new(current_stem))
            && library::is_video_file(candidate)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sibling_files(
    tx: &Connection,
    file_id: &str,
    library_id: Option<&str>,
    dir: &Path,
) -> Result<bool, StoreError> {
    let dir_key = crate::path_key::canonical_recorded_key(tx, &dir.to_string_lossy());
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_files
                       WHERE parent_dir_key = ?2 AND file_id <> ?1
                         AND (?3 IS NULL OR library_id = ?3))",
        params![file_id, dir_key, library_id],
        |r| r.get(0),
    )?;
    Ok(exists)
}

fn referenced_elsewhere(
    tx: &Connection,
    file_id: &str,
    library_id: Option<&str>,
    path: &Path,
) -> Result<bool, StoreError> {
    let path = crate::path_key::canonical_recorded_key(tx, &path.to_string_lossy());
    let used: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_files WHERE poster_path_key = ?2 AND file_id <> ?1 AND (?3 IS NULL OR library_id = ?3))
             OR EXISTS(SELECT 1 FROM library_files WHERE backdrop_path_key = ?2 AND file_id <> ?1 AND (?3 IS NULL OR library_id = ?3))
             OR EXISTS(SELECT 1 FROM library_files WHERE still_path_key = ?2 AND file_id <> ?1 AND (?3 IS NULL OR library_id = ?3))
             OR EXISTS(SELECT 1 FROM scrape_sidecars s WHERE s.path_key = ?2 AND s.file_id <> ?1
                         AND (?3 IS NULL OR EXISTS(SELECT 1 FROM library_files lf WHERE lf.file_id = s.file_id AND lf.library_id = ?3)))",
        params![file_id, path, library_id],
        |r| r.get(0),
    )?;
    Ok(used)
}

/// Disk phase for one sidecar, outside the Store lock. Idempotent for crash
/// recovery. The destination is created no-clobber; a competitor that appeared
/// after observation turns the decision into `KeepSource` (returned `true`).
pub(crate) fn apply_sidecar(side: &mut RenameSidecar) -> Result<bool, StoreError> {
    apply_sidecar_with_mode(side, false)
}

pub(crate) fn apply_sidecar_with_mode(
    side: &mut RenameSidecar,
    force_copy: bool,
) -> Result<bool, StoreError> {
    if matches!(
        side.action,
        SidecarAction::KeepSource | SidecarAction::AlreadyIdentical | SidecarAction::SameEntry
    ) || !side.from.is_file()
    {
        return Ok(false);
    }
    if super::rename_disk::same_directory_entry(&side.from, &side.to)? {
        side.action = SidecarAction::SameEntry;
        side.source_identity = None;
        side.target_identity = None;
        return Ok(false);
    }
    if let Some(parent) = side.to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut source_unlinked = false;
    let placed = match side.action {
        SidecarAction::Move if !force_copy => match std::fs::hard_link(&side.from, &side.to) {
            // Another filesystem: copy; the source is deleted at completion.
            Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => copy_sidecar(side),
            Ok(()) => {
                std::fs::remove_file(&side.from)?;
                source_unlinked = true;
                Ok(())
            }
            Err(error) => Err(error.into()),
        },
        _ => copy_sidecar(side),
    };
    if source_unlinked {
        // Source bytes are already gone; clear cleanup proof so a later file
        // created at `from` can never be mistaken for a leftover.
        side.source_identity = None;
    }
    match placed {
        Ok(()) => {
            side.target_identity = SidecarSourceIdentity::observe(&side.to);
            Ok(false)
        }
        Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if same_bytes(&side.from, &side.to).unwrap_or(false) {
                side.target_identity = SidecarSourceIdentity::observe(&side.to);
                return Ok(false);
            }
            tracing::warn!(from = %side.from.display(), to = %side.to.display(),
                "destination sidecar appeared after observation; not overwritten, source kept");
            side.action = SidecarAction::KeepSource;
            side.source_identity = None;
            side.target_identity = None;
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

fn copy_sidecar(side: &RenameSidecar) -> Result<(), StoreError> {
    let parent = side
        .to
        .parent()
        .ok_or_else(|| StoreError::Invalid("sidecar destination has no parent".into()))?;
    let mut source = std::fs::File::open(&side.from)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(&mut source, staged.as_file_mut())?;
    staged
        .as_file()
        .set_permissions(source.metadata()?.permissions())?;
    std::fs::hard_link(staged.path(), &side.to)?;
    Ok(())
}

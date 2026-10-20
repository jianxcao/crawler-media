use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use domain::{LedgerId, LedgerRow, LibraryId, MediaId, MediaKind, UnitKey};
use rusqlite::{OptionalExtension, params};

#[derive(Clone, Debug)]
pub struct ScannedLedger {
    pub row: LedgerRow,
    pub units: Vec<UnitKey>,
    pub source_version: String,
    pub source_digest: Option<String>,
    pub size_bytes: Option<i64>,
    pub created_ns: Option<i64>,
    pub modified_ns: Option<i64>,
    pub observation: Option<crate::metadata::NfoObservation>,
}


use super::library_write::reduce_incremental::{
    ChangedFile, read_file_summary_facts, reduce_library_media_incremental,
};
use super::library_write::{
    ensure_library_writable, ensure_media_writable, enqueue_probe, insert_file_row,
    reduce_library_media, sync_coverage,
};
use super::{Store, StoreError};

/// Maximum number of scanned files committed by one [`Store::insert_scanned_files`] call.
///
/// This is the scan commit window: every affected media is reduced once per window instead
/// of once per file.
pub const MAX_SCAN_BATCH: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanProbeObligation {
    pub ledger_id: LedgerId,
    pub created_at_ms: i64,
    pub reason: String,
}

/// How many files one batch published for a single media, keeping the file id and units
/// only while the media is still at exactly one changed file.
#[derive(Default)]
struct BatchChange {
    count: usize,
    file_id: Option<String>,
    units: Vec<(i32, i32)>,
}

impl Store {
    pub fn insert_scanned_files(&self, rows: &[ScannedLedger], now_ms: i64) -> Result<(), StoreError> {
        if rows.is_empty() { return Ok(()); }
        if rows.len() > MAX_SCAN_BATCH {
            return Err(StoreError::Invalid(format!("exceeded {MAX_SCAN_BATCH} files per scan batch")));
        }
        let tx = self.library.unchecked_transaction()?;
        let mut affected: HashMap<(String, MediaId), BatchChange> = HashMap::new();
        for scanned in rows {
            let row = &scanned.row;
            let kind = media_kind(&tx, row.media_id)?;
            let (library_id, root_id) = resolve_owned_root(&tx, Path::new(&row.path), kind)?;
            ensure_library_writable(&tx, &library_id)?;
            ensure_media_writable(&tx, row.media_id)?;
            let artwork = crate::files::DiscoveredArtwork::discover(Path::new(&row.path));
            let file = domain::LibraryFile {
                file_id: row.id, library_id: library_id.parse::<LibraryId>()?, root_id,
                media_id: row.media_id, kind, path: Path::new(&row.path).to_path_buf(),
                version: domain::FileVersion { revision: 1, source_version: scanned.source_version.clone() },
                presence: domain::FilePresence::Present, publication: domain::FilePublication::Published,
                size_bytes: scanned.size_bytes.and_then(|size| u64::try_from(size).ok()),
                stat_known: scanned.modified_ns.is_some(),
                created_ns: scanned.created_ns, modified_ns: scanned.modified_ns,
                source_digest: scanned.source_digest.clone(), source_path: None, transfer_mode: None,
                resolution: row.resolution.clone(), codec: row.codec.clone(), hdr: row.hdr.clone(),
                quality_source: row.quality_source, confidence: row.confidence, filter_score: row.filter_score,
                release_quality: None, duration_ms: None, tracks_version: 0,
                poster_path: artwork.poster, backdrop_path: artwork.backdrop, still_path: artwork.still,
            };
            insert_file_row(&tx, &file)?;
            let units = if kind == MediaKind::Video { vec![UnitKey::WHOLE] } else { scanned.units.clone() };
            sync_coverage(&tx, &file, &units)?;
            if let Some(observation) = &scanned.observation {
                if !crate::metadata::nfo_input::record_nfo_observation(&tx, &file, observation)? {
                    return Err(StoreError::Protected(format!("stale NFO observation for {}", row.path)));
                }
            }
            enqueue_probe(&tx, file.file_id, now_ms)?;
            let change = affected.entry((library_id, row.media_id)).or_default();
            change.count += 1;
            if change.count == 1 {
                change.file_id = Some(file.file_id.to_string());
                change.units = units.iter().map(|unit| (unit.season, unit.episode)).collect();
            } else { change.file_id = None; change.units.clear(); }
        }
        for ((library_id, media_id), change) in affected {
            let library_id = library_id.parse::<LibraryId>()?;
            if let Some(file_id) = change.file_id {
                let after = read_file_summary_facts(&tx, &file_id)?
                    .ok_or_else(|| StoreError::Missing(format!("file {file_id}")))?;
                reduce_library_media_incremental(&tx, library_id, media_id, &ChangedFile {
                    before: None, before_units: &[], after: &after, after_units: &change.units,
                })?;
            } else { reduce_library_media(&tx, library_id, media_id)?; }
        }
        tx.commit()?;
        Ok(())
    }
}

impl Store {
    /// Retrieve up to `limit` (clamped 1..=100) pending probe obligations in deterministic order.
    pub fn scan_probe_obligations(&self, limit: usize) -> Result<Vec<ScanProbeObligation>, StoreError> {
        self.scan_probe_obligations_with_vm_steps(limit).map(|(obligations, _)| obligations)
    }

    pub fn scan_probe_obligations_with_vm_steps(&self, limit: usize) -> Result<(Vec<ScanProbeObligation>, i64), StoreError> {
        let limit = limit.clamp(1, 100);
        let mut stmt = self.library.prepare("SELECT file_id, created_at_ms, reason FROM file_probe_obligations ORDER BY created_at_ms ASC, file_id ASC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit], |row| {
            let id: String = row.get(0)?;
            Ok(ScanProbeObligation {
                ledger_id: LedgerId::from_str(&id).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?,
                created_at_ms: row.get(1)?, reason: row.get(2)?,
            })
        })?;
        let obligations = rows.collect::<Result<Vec<_>, _>>()?;
        let vm_steps = i64::from(stmt.get_status(rusqlite::StatementStatus::VmStep));
        Ok((obligations, vm_steps))
    }

    pub fn ack_scan_probe_obligation(&self, ledger_id: LedgerId) -> Result<(), StoreError> {
        self.library.execute("DELETE FROM file_probe_obligations WHERE file_id = ?1", params![ledger_id.to_string()])?;
        Ok(())
    }
}

fn media_kind(tx: &rusqlite::Connection, media_id: domain::MediaId) -> Result<MediaKind, StoreError> {
    let kind: String = tx
        .query_row(
            "SELECT kind FROM media WHERE media_id = ?1",
            params![media_id.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| StoreError::Missing(format!("media {media_id}")))?;
    kind.parse::<MediaKind>().map_err(StoreError::MediaKind)
}

/// Longest enabled root that component-wise contains `path` for this kind of Library.
///
/// Equal-depth candidates are ambiguous and therefore rejected; a path outside every
/// root is rejected too. Never falls back to an arbitrary Library.
fn resolve_owned_root(
    tx: &rusqlite::Connection,
    path: &Path,
    kind: MediaKind,
) -> Result<(String, String), StoreError> {
    let mut stmt = tx.prepare(
        "SELECT r.root_id, r.library_id, r.display_path
         FROM library_roots r
         JOIN libraries l ON l.library_id = r.library_id
         WHERE l.kind = ?1 AND l.lifecycle_state = 'ready'
           AND NOT EXISTS (SELECT 1 FROM library_removals m
                           WHERE m.library_id = r.library_id AND m.state <> 'succeeded')",
    )?;
    let rows = stmt
        .query_map(params![kind.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut best: Option<(usize, String, String)> = None;
    let mut ambiguous = false;
    for (root_id, library_id, display) in rows {
        let root = Path::new(&display);
        if !path.starts_with(root) {
            continue;
        }
        let depth = root.components().count();
        match &best {
            Some((best_depth, _, _)) if *best_depth > depth => {}
            Some((best_depth, _, _)) if *best_depth == depth => ambiguous = true,
            _ => {
                best = Some((depth, root_id, library_id));
                ambiguous = false;
            }
        }
    }
    if ambiguous {
        return Err(StoreError::Protected(format!(
            "path {} has ambiguous library ownership",
            path.display()
        )));
    }
    best.map(|(_, root_id, library_id)| (library_id, root_id))
        .ok_or_else(|| {
            StoreError::Protected(format!(
                "path {} is outside every {} library root",
                path.display(),
                kind.as_str()
            ))
        })
}

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use domain::{Confidence, LedgerId, LibraryId, MediaId, MediaKind, QualitySource, UnitKey};
use rusqlite::{OptionalExtension, params};

/// One file observed by a scan, before it becomes a `library_files` row.
///
/// The scan only asserts what it measured: identity, path, unit coordinates and the
/// probe-ish labels. Ownership is captured separately before disk IO, then validated in the
/// transaction by [`Store::insert_scanned_files_in_contexts`], so a caller cannot forge
/// presence, publication or version state.
#[derive(Clone, Debug)]
pub struct ScannedLedger {
    pub file_id: LedgerId,
    pub media_id: MediaId,
    pub path: String,
    pub units: Vec<UnitKey>,
    pub resolution: Option<String>,
    pub codec: Option<String>,
    pub hdr: Option<String>,
    pub quality_source: QualitySource,
    pub confidence: Confidence,
    pub filter_score: Option<i32>,
    pub source_version: String,
    pub source_digest: Option<String>,
    pub size_bytes: Option<i64>,
    pub created_ns: Option<i64>,
    pub modified_ns: Option<i64>,
    /// Artwork already observed before acquiring the Store mutex.
    pub artwork: crate::files::DiscoveredArtwork,
    /// 该文件的全部 NFO 候选观察（每份存在的 sidecar 一条；不存在的候选也会出现，用来解除旧输入）。
    pub observations: Vec<crate::metadata::NfoObservation>,
}

use super::library_write::reduce_incremental::{
    ChangedFile, read_file_summary_facts, reduce_library_media_incremental,
};
use super::library_write::{
    enqueue_probe, ensure_media_writable, insert_file_row, reduce_library_media, sync_coverage,
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
    pub fn insert_scanned_files(
        &self,
        rows: &[ScannedLedger],
        now_ms: i64,
    ) -> Result<(), StoreError> {
        if rows.len() > MAX_SCAN_BATCH {
            return Err(StoreError::Invalid(format!(
                "exceeded {MAX_SCAN_BATCH} files per scan batch"
            )));
        }
        let contexts = rows
            .iter()
            .map(|row| {
                self.scan_write_context(
                    Path::new(&row.path),
                    media_kind(&self.library, row.media_id)?,
                )
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        self.insert_scanned_files_in_contexts(rows, &contexts, now_ms)
    }

    /// Publish only into the exact roots and configurations captured before disk IO.
    pub fn insert_scanned_files_in_contexts(
        &self,
        rows: &[ScannedLedger],
        contexts: &[crate::ScanWriteContext],
        now_ms: i64,
    ) -> Result<(), StoreError> {
        if rows.len() != contexts.len() {
            return Err(StoreError::Invalid(
                "scan context count does not match files".into(),
            ));
        }
        if rows.is_empty() {
            return Ok(());
        }
        if rows.len() > MAX_SCAN_BATCH {
            return Err(StoreError::Invalid(format!(
                "exceeded {MAX_SCAN_BATCH} files per scan batch"
            )));
        }
        let tx = crate::write_tx(&self.library)?;
        let mut affected: HashMap<(String, MediaId), BatchChange> = HashMap::new();
        for (scanned, context) in rows.iter().zip(contexts) {
            let kind = media_kind(&tx, scanned.media_id)?;
            context.validate(&tx, Path::new(&scanned.path), kind)?;
            let library_id = context.library_id.to_string();
            ensure_media_writable(&tx, scanned.media_id)?;
            let file = scanned_file(scanned, context);
            insert_file_row(&tx, &file)?;
            let units = if kind == MediaKind::Video {
                vec![UnitKey::WHOLE]
            } else {
                scanned.units.clone()
            };
            sync_coverage(&tx, &file, &units)?;
            let target = crate::metadata::nfo_input::NfoInputTarget::from_file(&file)
                .with_canonical_path(&tx);
            if !crate::metadata::nfo_input::record_nfo_observations(
                &tx,
                &target,
                &scanned.observations,
            )? {
                return Err(StoreError::Protected(format!(
                    "stale NFO observation for {}",
                    scanned.path
                )));
            }
            enqueue_probe(&tx, file.file_id, now_ms)?;
            let change = affected.entry((library_id, scanned.media_id)).or_default();
            change.count += 1;
            if change.count == 1 {
                change.file_id = Some(file.file_id.to_string());
                change.units = units
                    .iter()
                    .map(|unit| (unit.season, unit.episode))
                    .collect();
            } else {
                change.file_id = None;
                change.units.clear();
            }
        }
        for ((library_id, media_id), change) in affected {
            let library_id = library_id.parse::<LibraryId>()?;
            if let Some(file_id) = change.file_id {
                let after = read_file_summary_facts(&tx, &file_id)?
                    .ok_or_else(|| StoreError::Missing(format!("file {file_id}")))?;
                reduce_library_media_incremental(
                    &tx,
                    library_id,
                    media_id,
                    &ChangedFile {
                        before: None,
                        before_units: &[],
                        after: &after,
                        after_units: &change.units,
                    },
                )?;
            } else {
                reduce_library_media(&tx, library_id, media_id)?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

impl Store {
    /// Retrieve up to `limit` (clamped 1..=100) pending probe obligations in deterministic order.
    pub fn scan_probe_obligations(
        &self,
        limit: usize,
    ) -> Result<Vec<ScanProbeObligation>, StoreError> {
        self.scan_probe_obligations_with_vm_steps(limit)
            .map(|(obligations, _)| obligations)
    }

    pub fn scan_probe_obligations_with_vm_steps(
        &self,
        limit: usize,
    ) -> Result<(Vec<ScanProbeObligation>, i64), StoreError> {
        let limit = limit.clamp(1, 100);
        let mut stmt = self.library.prepare("SELECT file_id, created_at_ms, reason FROM file_probe_obligations ORDER BY created_at_ms ASC, file_id ASC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit], |row| {
            let id: String = row.get(0)?;
            Ok(ScanProbeObligation {
                ledger_id: LedgerId::from_str(&id)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?,
                created_at_ms: row.get(1)?,
                reason: row.get(2)?,
            })
        })?;
        let obligations = rows.collect::<Result<Vec<_>, _>>()?;
        let vm_steps = i64::from(stmt.get_status(rusqlite::StatementStatus::VmStep));
        Ok((obligations, vm_steps))
    }

    pub fn ack_scan_probe_obligation(&self, ledger_id: LedgerId) -> Result<(), StoreError> {
        self.library.execute(
            "DELETE FROM file_probe_obligations WHERE file_id = ?1",
            params![ledger_id.to_string()],
        )?;
        Ok(())
    }
}

fn media_kind(
    tx: &rusqlite::Connection,
    media_id: domain::MediaId,
) -> Result<MediaKind, StoreError> {
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

fn scanned_file(scanned: &ScannedLedger, context: &crate::ScanWriteContext) -> domain::LibraryFile {
    let artwork = &scanned.artwork;
    domain::LibraryFile {
        file_id: scanned.file_id,
        library_id: context.library_id,
        root_id: context.root_id.clone(),
        media_id: scanned.media_id,
        kind: context.kind,
        path: Path::new(&scanned.path).to_path_buf(),
        version: domain::FileVersion {
            revision: 1,
            source_version: scanned.source_version.clone(),
        },
        presence: domain::FilePresence::Present,
        publication: domain::FilePublication::Published,
        size_bytes: scanned.size_bytes.and_then(|size| u64::try_from(size).ok()),
        stat_known: scanned.modified_ns.is_some(),
        created_ns: scanned.created_ns,
        modified_ns: scanned.modified_ns,
        source_digest: scanned.source_digest.clone(),
        source_path: None,
        transfer_mode: None,
        resolution: scanned.resolution.clone(),
        codec: scanned.codec.clone(),
        hdr: scanned.hdr.clone(),
        quality_source: scanned.quality_source,
        confidence: scanned.confidence,
        filter_score: scanned.filter_score,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: artwork.poster.clone(),
        backdrop_path: artwork.backdrop.clone(),
        still_path: artwork.still.clone(),
    }
}

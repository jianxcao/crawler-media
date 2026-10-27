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

    /// Acknowledge the exact obligation the caller assessed, and only while its source is
    /// still recorded present. A newer obligation (the file was re-armed) or a file that
    /// presence has since marked missing keeps its durable work.
    pub fn ack_scan_probe_obligation_if_current(
        &self,
        obligation: &ScanProbeObligation,
    ) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "DELETE FROM file_probe_obligations
             WHERE file_id = ?1 AND created_at_ms = ?2 AND reason = ?3
               AND EXISTS (SELECT 1 FROM library_files f
                           WHERE f.file_id = ?1 AND f.missing_at IS NULL)",
            params![
                obligation.ledger_id.to_string(),
                obligation.created_at_ms,
                obligation.reason
            ],
        )?;
        Ok(changed > 0)
    }

    /// Acknowledge an unchanged obligation only once durable work proves acceptance.
    ///
    /// A known job may already have finished; an unspecified job must still be active, so a
    /// rejected persistence (no unit was created) never acknowledges the obligation.
    pub fn ack_scan_probe_obligation_for_work(
        &self,
        obligation: &ScanProbeObligation,
        job_id: Option<&str>,
    ) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "DELETE FROM file_probe_obligations
             WHERE file_id = ?1 AND created_at_ms = ?2 AND reason = ?3
               AND EXISTS (SELECT 1 FROM library_files f
                           WHERE f.file_id = ?1 AND f.missing_at IS NULL)
               AND EXISTS (
                   SELECT 1 FROM probe_job_units u JOIN probe_jobs j ON j.id = u.job_id
                   WHERE u.file_id = ?1 AND (
                       (?4 IS NOT NULL AND u.job_id = ?4
                        AND u.status IN ('queued', 'running', 'succeeded', 'failed', 'partial')
                        AND j.status IN ('queued', 'running', 'succeeded', 'failed', 'partial'))
                       OR (?4 IS NULL AND u.status IN ('queued', 'running')
                           AND j.status IN ('queued', 'running'))
                   )
               )",
            params![
                obligation.ledger_id.to_string(),
                obligation.created_at_ms,
                obligation.reason,
                job_id
            ],
        )?;
        Ok(changed > 0)
    }

    /// Queued/running probe units whose source presence is already recorded missing.
    ///
    /// Restart recovery calls this so a job persisted after the presence verdict (or before
    /// the process died) is retired instead of probing a source that is known to be gone.
    pub fn missing_source_probe_unit_refs(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, String)>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut statement = self.library.prepare(
            "SELECT u.job_id, u.file_id
             FROM probe_job_units u
             JOIN probe_jobs j ON j.id = u.job_id
             JOIN library_files f ON f.file_id = u.file_id
             WHERE u.status IN ('queued', 'running')
               AND j.status IN ('queued', 'running')
               AND f.missing_at IS NOT NULL
             ORDER BY u.rowid LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as i64], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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

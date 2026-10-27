//! Atomic file publication and presence maintenance.
//!
//! Every entry point validates the Library lifecycle, the file's root ownership and the
//! Media identity state before writing, then reduces the affected summaries in the same
//! transaction. Callers never refresh a card themselves.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use domain::{FileVersion, LedgerId, LibraryId, MediaId, MediaKind};
use rusqlite::{Connection, OptionalExtension, params};

use super::super::files::LibraryItemSummary;
use super::super::{Store, StoreError};
use super::publish_batch::{PendingChange, item_card_exists, reduce_publication_batch};
use super::reduce::{reduce_library_media, reduce_media_everywhere};
use super::reduce_incremental::{read_file_summary_facts, read_file_units};
use super::{FilePublication, PublishReport, WriteScope};

mod file_row;
mod stats;

use file_row::{RowPlan, plan_file_row, sorted_units, update_file_row};
pub(crate) use file_row::{enqueue_probe, insert_file_row, sync_coverage};
pub(crate) use stats::PublicationCounters;
#[cfg(feature = "test-support")]
pub use stats::PublishStats;

impl Store {
    /// Publish files (pending or published) into a Library.
    pub fn publish_files(
        &self,
        scope: &WriteScope,
        files: &[FilePublication<'_>],
        now_ms: i64,
    ) -> Result<PublishReport, StoreError> {
        self.publish_files_with_validation(scope, files, now_ms, |_| Ok(()))
    }

    pub(super) fn publish_files_with_validation(
        &self,
        scope: &WriteScope,
        files: &[FilePublication<'_>],
        now_ms: i64,
        validate: impl FnOnce(&Connection) -> Result<(), StoreError>,
    ) -> Result<PublishReport, StoreError> {
        self.publish_files_counted(scope, files, now_ms, validate, &mut PublicationCounters::default())
    }

    /// Measured publication: the same path, plus deterministic work counters.
    ///
    /// `test-support` only. An unchanged republish must write nothing, so the counters exist
    /// to assert that instead of timing it.
    ///
    /// Reference shape for measured assertions (Task 21): hidden from the docs, compiled only
    /// for tests, and `written_rows` comes from SQLite's own `total_changes` — never a counter
    /// the write path increments by hand, which would only ever prove the path agrees with
    /// itself.
    #[doc(hidden)]
    #[cfg(feature = "test-support")]
    pub fn publish_files_with_stats(
        &self,
        scope: &WriteScope,
        files: &[FilePublication<'_>],
        now_ms: i64,
    ) -> Result<(PublishReport, PublishStats), StoreError> {
        let mut counters = PublicationCounters::default();
        let written_before = self.library.total_changes();
        let report = self.publish_files_counted(scope, files, now_ms, |_| Ok(()), &mut counters)?;
        let mut stats = counters.into_stats();
        stats.written_rows = self.library.total_changes() - written_before;
        Ok((report, stats))
    }

    fn publish_files_counted(
        &self,
        scope: &WriteScope,
        files: &[FilePublication<'_>],
        now_ms: i64,
        validate: impl FnOnce(&Connection) -> Result<(), StoreError>,
        counters: &mut PublicationCounters,
    ) -> Result<PublishReport, StoreError> {
        if files.is_empty() {
            return Ok(PublishReport::default());
        }
        if files.len() > 100 {
            return Err(StoreError::Invalid(
                "exceeded 100 files per publication batch".into(),
            ));
        }
        let tx = crate::write_tx(&self.library)?;
        ensure_library_writable(&tx, &scope.library_id.to_string())?;
        ensure_config_revision(&tx, scope)?;
        validate(&tx)?;
        let mut state = BatchState::default();
        for pub_item in files {
            state.publish(&tx, scope, pub_item, now_ms, counters)?;
        }
        reduce_publication_batch(
            &tx,
            scope.library_id,
            std::mem::take(&mut state.affected),
            std::mem::take(&mut state.before_projection),
            counters,
        )?;
        tx.commit()?;
        Ok(state.report)
    }

    /// Recompute the summaries of every library that owns files of `media_id`.
    pub fn refresh_media_summaries(
        &self,
        media_id: MediaId,
        _now_ms: i64,
    ) -> Result<usize, StoreError> {
        let tx = crate::write_tx(&self.library)?;
        let reduced = reduce_media_everywhere(&tx, media_id)?;
        tx.commit()?;
        Ok(reduced)
    }

    pub fn get_library_item(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
    ) -> Result<Option<LibraryItemSummary>, StoreError> {
        let sql = "SELECT library_id, media_id, kind, title, file_count, available_file_count,
                          missing_file_count, unknown_file_count, distinct_episode_count,
                          distinct_season_count, preferred_file_id, size_bytes
                   FROM library_items
                   WHERE library_id = ?1 AND media_id = ?2";
        let mut stmt = self.library.prepare(sql)?;
        let row = stmt
            .query_row(params![library_id.to_string(), media_id.to_string()], |r| {
                let pref_str: Option<String> = r.get(10)?;
                let pref_id = pref_str.and_then(|s| s.parse::<LedgerId>().ok());
                let size_i: Option<i64> = r.get(11)?;
                let kind_str: String = r.get(2)?;
                let kind = kind_str.parse::<MediaKind>().unwrap_or(MediaKind::Movie);

                Ok(LibraryItemSummary {
                    library_id,
                    media_id,
                    kind,
                    title: r.get(3)?,
                    file_count: r.get::<_, i64>(4)? as usize,
                    available_file_count: r.get::<_, i64>(5)? as usize,
                    missing_file_count: r.get::<_, i64>(6)? as usize,
                    unknown_file_count: r.get::<_, i64>(7)? as usize,
                    distinct_episode_count: r.get::<_, i64>(8)? as usize,
                    distinct_season_count: r.get::<_, i64>(9)? as usize,
                    preferred_file_id: pref_id,
                    size_bytes: size_i.map(|s| s as u64),
                })
            })
            .optional()?;
        Ok(row)
    }

    pub fn count_probe_obligations(&self) -> Result<usize, StoreError> {
        let count: i64 =
            self.library
                .query_row("SELECT COUNT(*) FROM file_probe_obligations", [], |r| {
                    r.get(0)
                })?;
        Ok(count as usize)
    }

    /// Apply presence observations (verify or prune) and reduce the affected summaries.
    pub fn apply_presence(
        &self,
        scope: &WriteScope,
        observations: &[super::PresenceObservation],
        mode: super::PresenceMode,
        now_ms: i64,
    ) -> Result<super::PresenceReport, StoreError> {
        let tx = crate::write_tx(&self.library)?;
        ensure_library_writable(&tx, &scope.library_id.to_string())?;
        ensure_config_revision(&tx, scope)?;
        let mut report = super::PresenceReport::default();
        let mut affected: HashSet<MediaId> = HashSet::new();

        for obs in observations {
            let file_id_str = obs.file_id.to_string();
            let existing: Option<(String, String, i64, String, String, String)> = tx
                .query_row(
                    "SELECT f.media_id, f.presence_state, f.file_revision, f.source_version,
                            f.path_key, r.path_key
                     FROM library_files f JOIN library_roots r ON r.root_id = f.root_id
                     WHERE f.file_id = ?1 AND f.library_id = ?2",
                    params![file_id_str, scope.library_id.to_string()],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    },
                )
                .optional()?;

            let Some((media_id_str, current_state, cur_rev, cur_src_ver, cur_path, root_key)) =
                existing
            else {
                report.skipped += 1;
                continue;
            };

            if obs.observed_version.revision != cur_rev
                || obs.observed_version.source_version != cur_src_ver
                || crate::path_key::normalize_path_key(
                    &Path::new(&root_key)
                        .join(&obs.observed_path)
                        .to_string_lossy(),
                ) != cur_path
            {
                report.skipped += 1;
                continue;
            }
            let media_id = media_id_str.parse::<MediaId>()?;
            crate::library_operations::integrity_guard::ensure_media_integrity_writable(
                &tx, media_id,
            )?;
            match mode {
                super::PresenceMode::Verify => {
                    if current_state != obs.presence.as_str() {
                        let missing_at = if obs.presence == domain::FilePresence::Missing {
                            Some(now_ms)
                        } else {
                            None
                        };
                        tx.execute(
                            "UPDATE library_files
                             SET presence_state = ?1, missing_at = ?2
                             WHERE file_id = ?3",
                            params![obs.presence.as_str(), missing_at, file_id_str],
                        )?;
                        if obs.presence == domain::FilePresence::Missing {
                            report.missing += 1;
                        } else if obs.presence == domain::FilePresence::Present {
                            report.restored += 1;
                        }
                        affected.insert(media_id);
                    } else {
                        report.skipped += 1;
                    }
                }
                super::PresenceMode::Prune => {
                    if obs.presence == domain::FilePresence::Missing {
                        tx.execute(
                            "DELETE FROM library_files WHERE file_id = ?1",
                            params![file_id_str],
                        )?;
                        report.deleted += 1;
                        affected.insert(media_id);
                    } else {
                        report.skipped += 1;
                    }
                }
            }
        }

        for media_id in affected {
            reduce_library_media(&tx, scope.library_id, media_id)?;
        }
        tx.commit()?;
        Ok(report)
    }

    /// Store cold file details (tracks/chapters/fingerprints) after a source-version check.
    pub fn publish_file_details(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        details: &super::super::files::FileDetails,
        _now_ms: i64,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(&self.library)?;
        let file_id_str = file_id.to_string();
        ensure_file_version(&tx, &file_id_str, expected)?;
        let media: String = tx.query_row(
            "SELECT media_id FROM library_files WHERE file_id=?1",
            [&file_id_str],
            |r| r.get(0),
        )?;
        ensure_media_writable(&tx, media.parse()?)?;
        upsert_file_details(&tx, &file_id_str, details)?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_file_details(
        &self,
        file_id: LedgerId,
    ) -> Result<Option<super::super::files::FileDetails>, StoreError> {
        let file_id_str = file_id.to_string();
        let sql = "SELECT source_revision, source_version, tracks_version, video_json, audio_json,
                          subtitle_json, chapters_json, fingerprint_json, outro_fingerprint_json,
                          format_duration_ms
                   FROM file_details
                   WHERE file_id = ?1";
        let mut stmt = self.library.prepare(sql)?;
        let res = stmt
            .query_row(params![file_id_str], map_file_details)
            .optional()?;
        Ok(res)
    }
}

pub(crate) fn map_file_details(
    r: &rusqlite::Row<'_>,
) -> rusqlite::Result<super::super::files::FileDetails> {
    Ok(super::super::files::FileDetails {
        source_revision: r.get(0)?,
        source_version: r.get(1)?,
        tracks_version: r.get(2)?,
        video_json: r.get(3)?,
        audio_json: r.get(4)?,
        subtitle_json: r.get(5)?,
        chapters_json: r.get(6)?,
        fingerprint_json: r.get(7)?,
        outro_fingerprint_json: r.get(8)?,
        format_duration_ms: r.get(9)?,
    })
}

/// Reject writes to a Library that is frozen, draining, being removed or reconfigured.
pub(crate) fn ensure_library_writable(tx: &Connection, lib: &str) -> Result<(), StoreError> {
    let row: Option<(String, i64)> = tx
        .query_row(
            "SELECT lifecycle_state, config_revision FROM libraries WHERE library_id = ?1",
            params![lib],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((state, revision)) = row else {
        return Err(StoreError::Missing(format!("library {lib}")));
    };
    if state != "ready" {
        return Err(StoreError::Protected(format!(
            "library {lib} lifecycle is {state}"
        )));
    }
    let blocked: i64 = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_removals
                       WHERE library_id = ?1 AND state <> 'succeeded')",
        params![lib],
        |r| r.get(0),
    )?;
    if blocked != 0 {
        return Err(StoreError::Protected(format!(
            "library {lib} is being removed"
        )));
    }
    let _ = revision;
    Ok(())
}

/// Verify the caller's expected configuration revision. `0` is the real
/// initial revision of a Library, never an "unspecified" wildcard.
pub(crate) fn ensure_config_revision(
    tx: &Connection,
    scope: &WriteScope,
) -> Result<(), StoreError> {
    let current: i64 = tx.query_row(
        "SELECT config_revision FROM libraries WHERE library_id = ?1",
        params![scope.library_id.to_string()],
        |r| r.get(0),
    )?;
    if current != scope.config_revision {
        return Err(StoreError::Protected(format!(
            "library {} config revision changed: expected {}, current {current}",
            scope.library_id, scope.config_revision
        )));
    }
    Ok(())
}

/// Reject new file facts for a Media whose identity is frozen or retired.
pub(crate) fn ensure_media_writable(tx: &Connection, media_id: MediaId) -> Result<(), StoreError> {
    crate::library_operations::integrity_guard::ensure_media_integrity_writable(tx, media_id)?;
    let state: Option<String> = tx
        .query_row(
            "SELECT identity_state FROM media WHERE media_id = ?1",
            params![media_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    match state.as_deref() {
        None => Err(StoreError::Missing(format!("media {media_id}"))),
        Some("merging") | Some("retired") => Err(StoreError::Protected(format!(
            "media {media_id} identity is frozen"
        ))),
        _ => Ok(()),
    }
}

pub(super) fn ensure_file_version(
    tx: &Connection,
    file_id: &str,
    expected: &FileVersion,
) -> Result<(), StoreError> {
    let current: Option<(i64, String)> = tx
        .query_row(
            "SELECT file_revision, source_version FROM library_files WHERE file_id = ?1",
            params![file_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((revision, source_version)) = current else {
        return Err(StoreError::Missing(format!("file {file_id}")));
    };
    if revision != expected.revision || source_version != expected.source_version {
        return Err(StoreError::Protected(format!(
            "StaleVersion for file {file_id}: expected r{}v{}, got r{revision}v{source_version}",
            expected.revision, expected.source_version
        )));
    }
    Ok(())
}

pub(super) fn upsert_file_details(
    tx: &Connection,
    file_id: &str,
    details: &super::super::files::FileDetails,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO file_details
            (file_id, source_revision, source_version, tracks_version, video_json, audio_json,
             subtitle_json, chapters_json, fingerprint_json, outro_fingerprint_json,
             format_duration_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(file_id) DO UPDATE SET
            source_revision = excluded.source_revision,
            source_version = excluded.source_version,
            tracks_version = excluded.tracks_version,
            video_json = excluded.video_json,
            audio_json = excluded.audio_json,
            subtitle_json = excluded.subtitle_json,
            chapters_json = excluded.chapters_json,
            fingerprint_json = excluded.fingerprint_json,
            outro_fingerprint_json = excluded.outro_fingerprint_json,
            format_duration_ms = excluded.format_duration_ms",
        params![
            file_id,
            details.source_revision,
            details.source_version,
            details.tracks_version,
            details.video_json,
            details.audio_json,
            details.subtitle_json,
            details.chapters_json,
            details.fingerprint_json,
            details.outro_fingerprint_json,
            details.format_duration_ms,
        ],
    )?;
    Ok(())
}

/// Mutable state of one publication batch.
///
/// Accumulating here keeps the transaction's control flow (`open → validate → publish each →
/// settle → commit`) readable, and gives the per-file decision one home.
#[derive(Default)]
struct BatchState {
    report: PublishReport,
    affected: HashMap<MediaId, Vec<PendingChange>>,
    before_projection: HashMap<MediaId, crate::query_projection::ProjectionBefore>,
    /// Media whose card row is missing, decided once per batch. An unchanged file still
    /// repairs a missing card: the card is derived, not a fact the caller published.
    card_present: HashMap<MediaId, bool>,
}

impl BatchState {
    /// Publish one file: decide whether anything moved, then write only that.
    fn publish(
        &mut self,
        tx: &Connection,
        scope: &WriteScope,
        pub_item: &FilePublication<'_>,
        now_ms: i64,
        counters: &mut PublicationCounters,
    ) -> Result<(), StoreError> {
        let file = pub_item.file;
        if file.library_id != scope.library_id {
            return Err(StoreError::Invalid(
                "file library_id does not match write scope".into(),
            ));
        }
        // A root-relative path is resolved once here; every key is absolute.
        let absolutized = super::root_ownership::ensure_root_owns_path(tx, file)?;
        let file = absolutized.as_ref().unwrap_or(file);
        ensure_media_writable(tx, file.media_id)?;
        let file_id = file.file_id.to_string();
        let before = read_file_summary_facts(tx, &file_id)?;
        let before_units = read_file_units(tx, &file_id)?;
        let after_units = sorted_units(pub_item.units);
        let coverage_changed = before_units != after_units;
        // Decide the row write *before* touching anything else: an unchanged file owns no
        // summary work, so it must not even capture a projection it will never settle.
        let plan = plan_file_row(tx, file, pub_item.expected_version)?;
        let mut changed = !matches!(plan, RowPlan::Current) || coverage_changed;
        // NFO input feeds card facts, so it can change the card with an unchanged file row.
        changed |= pub_item.nfo_observation.is_some();
        if !changed {
            changed = !self.card_present(tx, scope, file.media_id)?;
        }
        if changed {
            // Captured before this media's first write; unchanged files write nothing, so
            // deferring to here still sees the batch-start projection.
            if let std::collections::hash_map::Entry::Vacant(entry) =
                self.before_projection.entry(file.media_id)
            {
                entry.insert(crate::query_projection::capture(
                    tx,
                    crate::query_projection::ProjectionKey {
                        library_id: scope.library_id,
                        media_id: file.media_id,
                    },
                )?);
            }
        }
        match plan {
            RowPlan::Insert => {
                insert_file_row(tx, file)?;
                self.report.inserted += 1;
            }
            RowPlan::Update {
                revision,
                report_unchanged,
            } => {
                update_file_row(tx, file, revision)?;
                if report_unchanged {
                    self.report.unchanged += 1;
                } else {
                    self.report.updated += 1;
                }
            }
            RowPlan::Current => self.report.unchanged += 1,
        }
        if coverage_changed {
            sync_coverage(tx, file, pub_item.units)?;
            counters.coverage_synced();
        }
        if let Some(observations) = pub_item.nfo_observation {
            let target =
                crate::metadata::nfo_input::NfoInputTarget::from_file(file).with_canonical_path(tx);
            if !crate::metadata::nfo_input::record_nfo_observations(tx, &target, observations)? {
                return Err(StoreError::Protected(format!(
                    "stale NFO observation for {}",
                    file.path.display()
                )));
            }
        }
        if pub_item.probe_required {
            enqueue_probe(tx, file.file_id, now_ms)?;
        }
        if !changed {
            // Nothing about this file or its coverage moved: no summary can change.
            counters.file_unchanged();
            return Ok(());
        }
        counters.file_changed();
        let after = read_file_summary_facts(tx, &file_id)?
            .ok_or_else(|| StoreError::Missing(format!("file {file_id}")))?;
        self.affected
            .entry(file.media_id)
            .or_default()
            .push(PendingChange {
                before,
                before_units,
                after,
                after_units,
            });
        Ok(())
    }

    /// Whether `(library, media)` already has a card row, asked once per batch.
    fn card_present(
        &mut self,
        tx: &Connection,
        scope: &WriteScope,
        media_id: MediaId,
    ) -> Result<bool, StoreError> {
        match self.card_present.entry(media_id) {
            std::collections::hash_map::Entry::Occupied(entry) => Ok(*entry.get()),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let present = item_card_exists(tx, scope.library_id, media_id)?;
                Ok(*entry.insert(present))
            }
        }
    }
}

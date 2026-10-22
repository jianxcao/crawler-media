//! Atomic file publication and presence maintenance.
//!
//! Every entry point validates the Library lifecycle, the file's root ownership and the
//! Media identity state before writing, then reduces the affected summaries in the same
//! transaction. Callers never refresh a card themselves.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use domain::{FileVersion, LedgerId, LibraryFile, LibraryId, MediaId, MediaKind, UnitKey};
use rusqlite::{Connection, OptionalExtension, params};

use super::super::files::LibraryItemSummary;
use super::super::{Store, StoreError};
use super::reduce::{reduce_library_media, reduce_media_everywhere};
use super::reduce_incremental::{
    BatchSettled, ChangedFile, FileSummaryFacts, read_file_summary_facts, read_file_units,
    reduce_library_media_incremental_in_batch,
};
use super::{FilePublication, PublishReport, WriteScope};

/// How one file row changed during a publication.
enum RowOutcome {
    Inserted,
    Updated,
    Unchanged,
}

/// The per-media outcome of a publication batch: a single changed file can be reduced
/// incrementally, anything else needs the full recompute.
/// Whether `(library, media)` already has a card row to move by deltas.
fn item_card_exists(
    tx: &Connection,
    library_id: LibraryId,
    media_id: MediaId,
) -> Result<bool, StoreError> {
    let found: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM library_items WHERE library_id = ?1 AND media_id = ?2",
            rusqlite::params![library_id.to_string(), media_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// One published file's before/after facts, kept so the reduction can apply per-file
/// deltas instead of re-reading every published file of the media once per window.
struct PendingChange {
    before: Option<FileSummaryFacts>,
    before_units: Vec<(i32, i32)>,
    after: FileSummaryFacts,
    after_units: Vec<(i32, i32)>,
}

impl Store {
    /// Publish files (pending or published) into a Library.
    pub fn publish_files(
        &self,
        scope: &WriteScope,
        files: &[FilePublication<'_>],
        now_ms: i64,
    ) -> Result<PublishReport, StoreError> {
        if files.is_empty() {
            return Ok(PublishReport::default());
        }
        if files.len() > 100 {
            return Err(StoreError::Invalid(
                "exceeded 100 files per publication batch".into(),
            ));
        }
        let tx = self.library.unchecked_transaction()?;
        ensure_library_writable(&tx, &scope.library_id.to_string())?;
        ensure_config_revision(&tx, scope)?;
        let mut report = PublishReport::default();
        let mut affected: HashMap<MediaId, Vec<PendingChange>> = HashMap::new();
        for pub_item in files {
            let file = pub_item.file;
            if file.library_id != scope.library_id {
                return Err(StoreError::Invalid(
                    "file library_id does not match write scope".into(),
                ));
            }
            ensure_root_owns_path(&tx, file)?;
            ensure_media_writable(&tx, file.media_id)?;
            let file_id = file.file_id.to_string();
            let before = read_file_summary_facts(&tx, &file_id)?;
            let before_units = read_file_units(&tx, &file_id)?;
            match upsert_file_row(&tx, file, pub_item.expected_version)? {
                RowOutcome::Inserted => report.inserted += 1,
                RowOutcome::Updated => report.updated += 1,
                RowOutcome::Unchanged => report.unchanged += 1,
            }
            sync_coverage(&tx, file, pub_item.units)?;
            if let Some(observations) = pub_item.nfo_observation {
                let target = crate::metadata::nfo_input::NfoInputTarget::from_file(file);
                if !crate::metadata::nfo_input::record_nfo_observations(&tx, &target, observations)?
                {
                    return Err(StoreError::Protected(format!(
                        "stale NFO observation for {}",
                        file.path.display()
                    )));
                }
            }
            if pub_item.probe_required {
                enqueue_probe(&tx, file.file_id, now_ms)?;
            }
            let after = read_file_summary_facts(&tx, &file_id)?
                .ok_or_else(|| StoreError::Missing(format!("file {file_id}")))?;
            let after_units: Vec<(i32, i32)> = pub_item
                .units
                .iter()
                .map(|unit| (unit.season, unit.episode))
                .collect();
            affected
                .entry(file.media_id)
                .or_default()
                .push(PendingChange {
                    before,
                    before_units,
                    after,
                    after_units,
                });
        }
        for (media_id, changes) in affected {
            // A media without a card yet cannot be moved by deltas: the incremental path
            // repairs it with a full reduce, and applying the remaining deltas afterwards
            // would count those files twice. Do the full reduce once and skip the rest.
            if !item_card_exists(&tx, scope.library_id, media_id)? {
                reduce_library_media(&tx, scope.library_id, media_id)?;
                continue;
            }
            // Apply one delta per changed file. A media that appears in several windows of a
            // scan used to fall back to a full `reduce_library_media` per window, which
            // re-reads every published file of that media: a 10k-episode series therefore
            // cost O(files x windows). Per-file deltas keep each window proportional to the
            // files it actually touched, and the totals end at the same place because the
            // deltas are applied in the same order the files were published.
            //
            // Every file of the batch is already written, so a unit (or card) that has
            // to be rebuilt from facts already counts the later files of the batch:
            // `settled` keeps those rows from receiving the same files again.
            let mut settled = BatchSettled::default();
            for change in changes {
                reduce_library_media_incremental_in_batch(
                    &tx,
                    scope.library_id,
                    media_id,
                    &ChangedFile {
                        before: change.before.as_ref(),
                        before_units: &change.before_units,
                        after: &change.after,
                        after_units: &change.after_units,
                    },
                    &mut settled,
                )?;
            }
        }
        tx.commit()?;
        Ok(report)
    }

    /// Recompute the summaries of every library that owns files of `media_id`.
    pub fn refresh_media_summaries(
        &self,
        media_id: MediaId,
        _now_ms: i64,
    ) -> Result<usize, StoreError> {
        let tx = self.library.unchecked_transaction()?;
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
        let tx = self.library.unchecked_transaction()?;
        ensure_library_writable(&tx, &scope.library_id.to_string())?;
        ensure_config_revision(&tx, scope)?;
        let mut report = super::PresenceReport::default();
        let mut affected: HashSet<MediaId> = HashSet::new();

        for obs in observations {
            let file_id_str = obs.file_id.to_string();
            let existing: Option<(String, String, i64, String, String)> = tx
                .query_row(
                    "SELECT media_id, presence_state, file_revision, source_version, path_key
                     FROM library_files
                     WHERE file_id = ?1 AND library_id = ?2",
                    params![file_id_str, scope.library_id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?;

            let Some((media_id_str, current_state, cur_rev, cur_src_ver, cur_path)) = existing
            else {
                report.skipped += 1;
                continue;
            };

            if obs.observed_version.revision != cur_rev
                || obs.observed_version.source_version != cur_src_ver
                || crate::path_key::normalize_path_key(&obs.observed_path.to_string_lossy())
                    != cur_path
            {
                report.skipped += 1;
                continue;
            }
            let media_id = media_id_str.parse::<MediaId>()?;
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
        let tx = self.library.unchecked_transaction()?;
        let file_id_str = file_id.to_string();
        ensure_file_version(&tx, &file_id_str, expected)?;
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

/// The declared root must exist for this Library, and an absolute path must live under it.
fn ensure_root_owns_path(tx: &Connection, file: &LibraryFile) -> Result<(), StoreError> {
    let root: Option<String> = tx
        .query_row(
            "SELECT display_path FROM library_roots WHERE root_id = ?1 AND library_id = ?2",
            params![file.root_id, file.library_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(root) = root else {
        return Err(StoreError::Invalid(format!(
            "root {} does not belong to library {}",
            file.root_id, file.library_id
        )));
    };
    if file.path.is_absolute() && !file.path.starts_with(Path::new(&root)) {
        return Err(StoreError::Invalid(format!(
            "path {} is outside root {root}",
            file.path.display()
        )));
    }
    Ok(())
}

fn upsert_file_row(
    tx: &Connection,
    file: &LibraryFile,
    expected: Option<&FileVersion>,
) -> Result<RowOutcome, StoreError> {
    let file_id = file.file_id.to_string();
    let current = tx
        .query_row(
            "SELECT file_revision, source_version, presence_state, publication_state, size_bytes,
                    resolution, codec, hdr, quality_source, confidence, filter_score
             FROM library_files WHERE file_id = ?1",
            params![file_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, String>(9)?,
                    r.get::<_, Option<i32>>(10)?,
                ))
            },
        )
        .optional()?;

    let Some(current) = current else {
        insert_file_row(tx, file)?;
        return Ok(RowOutcome::Inserted);
    };

    let (
        cur_rev,
        cur_source,
        cur_presence,
        cur_publication,
        cur_size,
        cur_resolution,
        cur_codec,
        cur_hdr,
        cur_quality_source,
        cur_confidence,
        cur_score,
    ) = current;
    if let Some(exp) = expected
        && (exp.revision != cur_rev || exp.source_version != cur_source)
    {
        return Err(StoreError::Protected(format!(
            "StaleVersion for file {file_id}: expected r{}v{}, got r{cur_rev}v{cur_source}",
            exp.revision, exp.source_version
        )));
    }
    // Only a content/source change advances the revision, so resume bindings stay valid
    // across metadata, presence and artwork refreshes.
    let bump_revision = cur_source != file.version.source_version;
    let new_revision = if bump_revision { cur_rev + 1 } else { cur_rev };
    let unchanged = !bump_revision
        && cur_presence == file.presence.as_str()
        && cur_publication == file.publication.as_str()
        && cur_size == file.size_bytes.map(|s| s as i64)
        && cur_resolution == file.resolution
        && cur_codec == file.codec
        && cur_hdr == file.hdr
        && cur_quality_source == file.quality_source.as_str()
        && cur_confidence == file.confidence.as_str()
        && cur_score == file.filter_score;
    update_file_row(tx, file, new_revision)?;
    Ok(if unchanged {
        RowOutcome::Unchanged
    } else {
        RowOutcome::Updated
    })
}

pub(crate) fn insert_file_row(tx: &Connection, file: &LibraryFile) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO library_files
            (file_id, library_id, root_id, media_id, kind, path_key, display_path,
             presence_state, publication_state, file_revision, source_version,
             size_bytes, stat_known, mtime_ns, created_ns, source_digest,
             source_path, transfer_mode, resolution, codec, hdr, quality_source, confidence,
             filter_score, release_quality, duration_ms, tracks_version,
             poster_path, backdrop_path, still_path, has_poster, has_backdrop, has_still)
         VALUES
            (?1, ?2, ?3, ?4, ?5, norm_path_key(?6), ?6,
             ?7, ?8, ?9, ?10,
             ?11, ?12, ?13, ?14, ?15,
             ?16, ?17, ?18, ?19, ?20, ?21, ?22,
             ?23, ?24, ?25, ?26,
             ?27, ?28, ?29, ?30, ?31, ?32)",
        params![
            file.file_id.to_string(),
            file.library_id.to_string(),
            file.root_id,
            file.media_id.to_string(),
            file.kind.as_str(),
            file.path.to_string_lossy(),
            file.presence.as_str(),
            file.publication.as_str(),
            file.version.revision,
            file.version.source_version,
            file.size_bytes.map(|s| s as i64),
            i64::from(file.stat_known),
            file.modified_ns,
            file.created_ns,
            file.source_digest,
            file.source_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            file.transfer_mode,
            file.resolution,
            file.codec,
            file.hdr,
            file.quality_source.as_str(),
            file.confidence.as_str(),
            file.filter_score,
            file.release_quality,
            file.duration_ms,
            file.tracks_version,
            file.poster_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            file.backdrop_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            file.still_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            i64::from(file.poster_path.is_some()),
            i64::from(file.backdrop_path.is_some()),
            i64::from(file.still_path.is_some()),
        ],
    )?;
    Ok(())
}

fn update_file_row(tx: &Connection, file: &LibraryFile, revision: i64) -> Result<(), StoreError> {
    // Artwork paths are never cleared by a later publication that does not carry them,
    // and a stat observation never overwrites a known value with NULL.
    tx.execute(
        "UPDATE library_files SET
            path_key = norm_path_key(?1), display_path = ?1,
            presence_state = ?2, publication_state = ?3, file_revision = ?4, source_version = ?5,
            size_bytes = COALESCE(?6, size_bytes), stat_known = MAX(stat_known, ?7),
            mtime_ns = COALESCE(?8, mtime_ns), created_ns = COALESCE(?9, created_ns),
            source_digest = COALESCE(?10, source_digest),
            source_path = COALESCE(?11, source_path),
            transfer_mode = COALESCE(?12, transfer_mode),
            resolution = COALESCE(?13, resolution), codec = COALESCE(?14, codec),
            hdr = COALESCE(?15, hdr), quality_source = ?16, confidence = ?17,
            filter_score = COALESCE(?18, filter_score),
            release_quality = COALESCE(?19, release_quality),
            duration_ms = COALESCE(?20, duration_ms), tracks_version = ?21,
            poster_path = COALESCE(?22, poster_path),
            backdrop_path = COALESCE(?23, backdrop_path),
            still_path = COALESCE(?24, still_path),
            has_poster = MAX(has_poster, ?25),
            has_backdrop = MAX(has_backdrop, ?26),
            has_still = MAX(has_still, ?27)
         WHERE file_id = ?28",
        params![
            file.path.to_string_lossy(),
            file.presence.as_str(),
            file.publication.as_str(),
            revision,
            file.version.source_version,
            file.size_bytes.map(|s| s as i64),
            i64::from(file.stat_known),
            file.modified_ns,
            file.created_ns,
            file.source_digest,
            file.source_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            file.transfer_mode,
            file.resolution,
            file.codec,
            file.hdr,
            file.quality_source.as_str(),
            file.confidence.as_str(),
            file.filter_score,
            file.release_quality,
            file.duration_ms,
            file.tracks_version,
            file.poster_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            file.backdrop_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            file.still_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            i64::from(file.poster_path.is_some()),
            i64::from(file.backdrop_path.is_some()),
            i64::from(file.still_path.is_some()),
            file.file_id.to_string(),
        ],
    )?;
    Ok(())
}

pub(crate) fn sync_coverage(
    tx: &Connection,
    file: &LibraryFile,
    units: &[UnitKey],
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM library_file_units WHERE file_id = ?1",
        params![file.file_id.to_string()],
    )?;
    for unit in units {
        tx.execute(
            "INSERT INTO library_file_units (file_id, library_id, media_id, season, episode)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                file.file_id.to_string(),
                file.library_id.to_string(),
                file.media_id.to_string(),
                unit.season,
                unit.episode,
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn enqueue_probe(
    tx: &Connection,
    file_id: LedgerId,
    now_ms: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO file_probe_obligations (file_id, created_at_ms, reason)
         VALUES (?1, ?2, 'publish')
         ON CONFLICT(file_id) DO NOTHING",
        params![file_id.to_string(), now_ms],
    )?;
    Ok(())
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

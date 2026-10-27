//! Probe result publication.
//!
//! A probe result describes one generation of one file: the stream facts it read are only
//! true while `library_files` still carries the revision and source version the worker
//! probed. A rescan that replaced the file (or a deletion) makes the result stale, so it is
//! refused with `Ok(false)` and the caller drops it instead of writing facts that belong to
//! a previous generation.
//!
//! Publishes in one transaction: hot quality columns (`resolution`/`codec`/`hdr`/
//! `duration_ms`/`tracks_version`), the cold `file_details` row, and the derived summaries of
//! every media the file belongs to. Stream facts cached by other stages (chapters, voice
//! fingerprints) survive a probe of the *same* source version; they are dropped when the
//! publication is for different content, because they describe the old bytes.

use domain::{FileVersion, LedgerId, LibraryId, MediaId};
use rusqlite::{Connection, OptionalExtension, params};

use super::publish::{map_file_details, upsert_file_details};
use super::reduce::reduce_library_media;
use crate::files::FileDetails;
use crate::{Store, StoreError};

/// Generation of the cached stream facts. `file_details.tracks_version` must be at least this
/// value for the cached tracks to be served, so every writer tags its row with it.
pub const CURRENT_TRACKS_VERSION: i64 = crate::library::CURRENT_TRACKS_VERSION;

/// Distinguish an unobserved field from an observed empty value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observed<T> {
    NotObserved,
    Value(T),
}

/// One probe outcome for one file.
pub struct ProbePublication<'a> {
    /// Version the worker probed. The publication is refused when the row moved on.
    pub expected: &'a FileVersion,
    pub tracks_version: i64,
    pub resolution: Option<&'a str>,
    pub codec: Option<&'a str>,
    pub hdr: Observed<Option<&'a str>>,
    pub duration_ms: Option<i64>,
    /// Cold stream facts. `None` publishes the hot quality columns only.
    pub details: Option<&'a FileDetails>,
}

const SELECT_FILE_VERSION: &str =
    "SELECT file_revision, source_version FROM library_files WHERE file_id = ?1";

const SELECT_DETAILS: &str = "SELECT source_revision, source_version, tracks_version, video_json,
                                     audio_json, subtitle_json, chapters_json, fingerprint_json,
                                     outro_fingerprint_json, format_duration_ms
                              FROM file_details WHERE file_id = ?1";

impl Store {
    /// Current version of a file row; `None` once the row is gone.
    pub fn get_file_version(&self, file_id: LedgerId) -> Result<Option<FileVersion>, StoreError> {
        self.library
            .query_row(
                SELECT_FILE_VERSION,
                params![file_id.to_string()],
                map_version,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Publish one probe result.
    ///
    /// Returns `false` when the file row is gone or no longer carries `p.expected`: the probe
    /// worker observed a file that is not the current one, so nothing is written.
    pub fn publish_probe_result(
        &self,
        file_id: LedgerId,
        p: &ProbePublication<'_>,
        _now_ms: i64,
    ) -> Result<bool, StoreError> {
        let tx = crate::write_tx(&self.library)?;
        let id = file_id.to_string();
        let Some((library_id, media_id)) = file_owner_tx(&tx, &id)? else {
            return Ok(false);
        };
        if current_version_tx(&tx, &id)? != Some(p.expected.clone()) {
            return Ok(false);
        }
        crate::library_operations::integrity_guard::ensure_media_integrity_writable(&tx, media_id)?;
        write_quality_tx(&tx, &id, p)?;
        if let Some(details) = p.details {
            let merged = merge_details_tx(&tx, &id, p.expected, details)?;
            upsert_file_details(&tx, &id, &merged)?;
        }
        reduce_library_media(&tx, library_id, media_id)?;
        tx.commit()?;
        Ok(true)
    }
}

fn map_version(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileVersion> {
    Ok(FileVersion {
        revision: row.get(0)?,
        source_version: row.get(1)?,
    })
}

fn current_version_tx(tx: &Connection, file_id: &str) -> Result<Option<FileVersion>, StoreError> {
    Ok(tx
        .query_row(SELECT_FILE_VERSION, params![file_id], map_version)
        .optional()?)
}

fn file_owner_tx(
    tx: &Connection,
    file_id: &str,
) -> Result<Option<(LibraryId, MediaId)>, StoreError> {
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT library_id, media_id FROM library_files WHERE file_id = ?1",
            params![file_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((library, media)) => Ok(Some((library.parse()?, media.parse()?))),
        None => Ok(None),
    }
}

/// Hot quality columns. Only the probe supplies real stream facts, so a probed value wins
/// over a release-name guess while `quality_source` records where the columns came from.
fn write_quality_tx(
    tx: &Connection,
    file_id: &str,
    p: &ProbePublication<'_>,
) -> Result<(), StoreError> {
    let (hdr_observed, hdr) = match p.hdr {
        Observed::NotObserved => (false, None),
        Observed::Value(value) => (true, value),
    };
    let probed = p.resolution.is_some() || p.codec.is_some() || hdr_observed;
    let changed = tx.execute(
        "UPDATE library_files SET
             resolution = COALESCE(?2, resolution),
             codec = COALESCE(?3, codec),
             hdr = CASE WHEN ?8 THEN ?4 ELSE hdr END,
             hdr_observed = MAX(hdr_observed, ?8),
             duration_ms = COALESCE(?5, duration_ms),
             tracks_version = MAX(tracks_version, ?6),
             quality_source = CASE WHEN ?7 = 1 THEN 'probe' ELSE quality_source END
         WHERE file_id = ?1",
        params![
            file_id,
            p.resolution,
            p.codec,
            hdr,
            p.duration_ms,
            p.tracks_version,
            i64::from(probed),
            hdr_observed,
        ],
    )?;
    if changed == 0 {
        return Err(StoreError::Missing(format!("file {file_id}")));
    }
    Ok(())
}

/// Cold stream facts, keeping what other stages cached for the same source version.
fn merge_details_tx(
    tx: &Connection,
    file_id: &str,
    expected: &FileVersion,
    details: &FileDetails,
) -> Result<FileDetails, StoreError> {
    if details.source_revision != expected.revision
        || details.source_version != expected.source_version
    {
        return Err(StoreError::Invalid(format!(
            "probe details for file {file_id} carry r{}v{} but the probe pinned r{}v{}",
            details.source_revision,
            details.source_version,
            expected.revision,
            expected.source_version
        )));
    }
    let existing: Option<FileDetails> = tx
        .query_row(SELECT_DETAILS, params![file_id], map_file_details)
        .optional()?;
    // Different content: chapters, fingerprints and the old video stream all describe bytes
    // that are no longer there, so the probe's own values replace them.
    let Some(existing) = existing.filter(|row| row.source_version == details.source_version) else {
        return Ok(details.clone());
    };
    Ok(FileDetails {
        video_json: details.video_json.clone().or(existing.video_json),
        chapters_json: details.chapters_json.clone().or(existing.chapters_json),
        fingerprint_json: details
            .fingerprint_json
            .clone()
            .or(existing.fingerprint_json),
        outro_fingerprint_json: details
            .outro_fingerprint_json
            .clone()
            .or(existing.outro_fingerprint_json),
        format_duration_ms: details.format_duration_ms.or(existing.format_duration_ms),
        tracks_version: details.tracks_version.max(existing.tracks_version),
        audio_json: details.audio_json.clone(),
        subtitle_json: details.subtitle_json.clone(),
        source_revision: details.source_revision,
        source_version: details.source_version.clone(),
    })
}

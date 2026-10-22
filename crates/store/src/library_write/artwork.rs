//! Artwork facts on `library_files`.
//!
//! A file's artwork is published under the same version check as probe results, so a
//! late scrape worker cannot attach artwork to a file that was replaced underneath it.
//! The affected card is reduced in the same transaction.

use std::path::Path;

use domain::{FileVersion, LedgerId, LibraryId, MediaId};
use rusqlite::{Connection, OptionalExtension, params};

use super::super::{Store, StoreError};
use super::publish::ensure_file_version;
use super::reduce::reduce_library_media;
use super::reduce_incremental::{
    ChangedFile, read_file_summary_facts, read_file_units, reduce_library_media_incremental,
};

/// Artwork discovered beside one published file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileArtwork<'a> {
    pub poster: Option<&'a Path>,
    pub backdrop: Option<&'a Path>,
    pub still: Option<&'a Path>,
}

impl FileArtwork<'_> {
    fn is_empty(&self) -> bool {
        self.poster.is_none() && self.backdrop.is_none() && self.still.is_none()
    }
}

impl Store {
    /// Record artwork discovered beside one file and reduce its card.
    ///
    /// `expected` pins the file version the discovery observed; a stale version is
    /// rejected instead of writing artwork onto a replaced file. Only columns that
    /// receive a path are set, so a later observation without artwork never erases a
    /// known path.
    pub fn record_file_artwork(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        artwork: &FileArtwork<'_>,
    ) -> Result<bool, StoreError> {
        let tx = self.library.unchecked_transaction()?;
        let changed = record_file_artwork_in(&tx, file_id, expected, artwork)?;
        tx.commit()?;
        Ok(changed)
    }
}

/// The version-checked write; the caller owns the transaction and the commit.
pub(crate) fn record_file_artwork_in(
    tx: &Connection,
    file_id: LedgerId,
    expected: &FileVersion,
    artwork: &FileArtwork<'_>,
) -> Result<bool, StoreError> {
    let file_id_str = file_id.to_string();
    ensure_file_version(tx, &file_id_str, expected)?;
    if artwork.is_empty() {
        return Ok(false);
    }
    let target: Option<(String, String)> = tx
        .query_row(
            "SELECT library_id, media_id FROM library_files WHERE file_id = ?1",
            params![file_id_str],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (library_id, media_id) =
        target.ok_or_else(|| StoreError::Missing(format!("file {file_id}")))?;
    let library_id: LibraryId = library_id.parse()?;
    let media_id: MediaId = media_id.parse()?;

    let before = read_file_summary_facts(tx, &file_id_str)?;
    tx.execute(
        "UPDATE library_files SET
            poster_path = COALESCE(?1, poster_path),
            backdrop_path = COALESCE(?2, backdrop_path),
            still_path = COALESCE(?3, still_path),
            has_poster = MAX(has_poster, ?4),
            has_backdrop = MAX(has_backdrop, ?5),
            has_still = MAX(has_still, ?6)
         WHERE file_id = ?7",
        params![
            artwork
                .poster
                .map(|path| path.to_string_lossy().to_string()),
            artwork
                .backdrop
                .map(|path| path.to_string_lossy().to_string()),
            artwork.still.map(|path| path.to_string_lossy().to_string()),
            i64::from(artwork.poster.is_some()),
            i64::from(artwork.backdrop.is_some()),
            i64::from(artwork.still.is_some()),
            file_id_str,
        ],
    )?;

    let Some(before) = before else {
        reduce_library_media(tx, library_id, media_id)?;
        return Ok(true);
    };
    let Some(after) = read_file_summary_facts(tx, &file_id_str)? else {
        return Ok(false);
    };
    if after.published {
        let units = read_file_units(tx, &file_id_str)?;
        reduce_library_media_incremental(
            tx,
            library_id,
            media_id,
            &ChangedFile {
                before: Some(&before),
                before_units: &units,
                after: &after,
                after_units: &units,
            },
        )?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests;

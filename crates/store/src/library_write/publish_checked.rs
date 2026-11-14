//! Publication of a delayed observation checks its original identity in the write transaction.

use domain::LibraryFile;
use rusqlite::{Connection, OptionalExtension, params};

use super::{FilePublication, PublishReport, WriteScope};
use crate::{Store, StoreError};

impl Store {
    /// Publish one delayed observation only while its recorded context still matches.
    /// `None` asserts both the destination and file identity were absent. `Some` pins
    /// file/Library/root/Media/kind/path and source generation. The comparison and
    /// publication share one IMMEDIATE transaction, including across Store connections.
    pub fn publish_file_checked_context(
        &self,
        scope: &WriteScope,
        publication: &FilePublication<'_>,
        expected: Option<&LibraryFile>,
        now_ms: i64,
    ) -> Result<PublishReport, StoreError> {
        self.with_library_write(|db| {
            db.publish_file_checked_context(scope, publication, expected, now_ms)
        })
    }
    /// Consume the originally observed parked identity atomically with the file.
    pub fn publish_unidentified_checked_context(
        &self,
        scope: &WriteScope,
        publication: &FilePublication<'_>,
        expected: Option<&LibraryFile>,
        source: &crate::UnidentifiedRow,
        now_ms: i64,
    ) -> Result<PublishReport, StoreError> {
        self.with_library_write(|db| {
            db.publish_unidentified_checked_context(scope, publication, expected, source, now_ms)
        })
    }
}

pub(crate) fn validate_context(
    tx: &Connection,
    file: &LibraryFile,
    expected: Option<&LibraryFile>,
) -> Result<(), StoreError> {
    let key = crate::path_key::canonical_recorded_key(tx, &file.path.to_string_lossy());
    let matches = if let Some(expected) = expected {
        let expected_key =
            crate::path_key::canonical_recorded_key(tx, &expected.path.to_string_lossy());
        expected.file_id == file.file_id
            && expected.library_id == file.library_id
            && expected.root_id == file.root_id
            && expected.media_id == file.media_id
            && expected.kind == file.kind
            && expected_key == key
            && tx
                .query_row(
                    "SELECT library_id=?2 AND root_id=?3 AND media_id=?4 AND kind=?5
                        AND path_key=?6 AND file_revision=?7 AND source_version=?8
                 FROM library_files WHERE file_id=?1",
                    params![
                        expected.file_id.to_string(),
                        expected.library_id.to_string(),
                        expected.root_id,
                        expected.media_id.to_string(),
                        expected.kind.as_str(),
                        expected_key,
                        expected.version.revision,
                        expected.version.source_version
                    ],
                    |row| row.get::<_, bool>(0),
                )
                .optional()?
                .unwrap_or(false)
    } else {
        !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM library_files WHERE file_id=?1)
                 OR EXISTS(SELECT 1 FROM library_files WHERE path_key=?2)",
            params![file.file_id.to_string(), key],
            |row| row.get::<_, bool>(0),
        )?
    };
    if !matches {
        return Err(StoreError::Protected(format!(
            "publication context changed for {}",
            file.path.display()
        )));
    }
    Ok(())
}

#[path = "publish_checked/connection.rs"]
mod connection;

//! Work-scope NFO sidecar facts recorded as a Library's metadata input.
//!
//! The browse summary and the detail view read `library_metadata_inputs`, so the sidecar is
//! recorded inside the publication transaction. A card and its filters then share one view
//! of the Library's own NFO fields instead of only the adopted catalogue row.

use std::path::PathBuf;

use domain::{EntityScope, LibraryFile, MediaKind, MetadataFields};
use library::nfo::{NfoReadOutcome, parse_nfo_fields};
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;

/// The work-scope NFO sidecar of a published file, most specific candidate first.
fn nfo_sidecar(file: &LibraryFile) -> Option<PathBuf> {
    let directory = file.path.parent()?;
    let stem = file.path.file_stem()?.to_str()?;
    let candidates = match file.kind {
        MediaKind::Tv => {
            let in_season_directory = directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.to_ascii_lowercase().starts_with("season"));
            let series = if in_season_directory {
                directory.parent().unwrap_or(directory)
            } else {
                directory
            };
            vec![series.join("tvshow.nfo")]
        }
        _ => vec![
            directory.join(format!("{stem}.nfo")),
            directory.join("movie.nfo"),
        ],
    };
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// Record the work-scope facts of this file's NFO sidecar as the Library's metadata input.
pub(crate) fn record_nfo_input(tx: &Connection, file: &LibraryFile) -> Result<(), StoreError> {
    let Some(sidecar) = nfo_sidecar(file) else {
        clear_nfo_input(tx, file)?;
        return Ok(());
    };
    let body = match std::fs::read_to_string(&sidecar) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            clear_nfo_input(tx, file)?;
            return Ok(());
        }
        Err(error) => {
            // Unknown IO is not a deletion: keep the last valid input.
            tracing::warn!(
                file_id = %file.file_id,
                path = %sidecar.display(),
                %error,
                "NFO sidecar unreadable; keeping the last valid metadata input"
            );
            return Ok(());
        }
    };
    let (parse_state, fields) = match parse_nfo_fields(&body, EntityScope::Work) {
        NfoReadOutcome::Valid(input) => ("valid", input.fields),
        NfoReadOutcome::Invalid { message } => {
            tracing::warn!(file_id = %file.file_id, path = %sidecar.display(), %message,
                "NFO sidecar is invalid");
            ("invalid", MetadataFields::default())
        }
        NfoReadOutcome::Unknown { message } => {
            tracing::info!(file_id = %file.file_id, path = %sidecar.display(), %message,
                "NFO sidecar has an unsupported root");
            ("unknown", MetadataFields::default())
        }
        NfoReadOutcome::Missing => {
            clear_nfo_input(tx, file)?;
            return Ok(());
        }
    };
    let locale: String = tx
        .query_row(
            "SELECT display_locale FROM libraries WHERE library_id = ?1",
            params![file.library_id.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| "zh-CN".into());
    let fields_json = serde_json::to_string(&fields)?;
    let present: Vec<String> = serde_json::to_value(&fields)?
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter(|(_, value)| {
                    value.get("state").and_then(|state| state.as_str()) == Some("value")
                })
                .map(|(name, _)| name.clone())
                .collect()
        })
        .unwrap_or_default();
    let present_fields_json = serde_json::to_string(&present)?;
    let path_key = file.path.to_string_lossy();
    tx.execute(
        "INSERT INTO library_metadata_inputs
            (library_id, media_id, entity_scope, season, episode, locale, input_key,
             root_id, path_key, parse_state, source_version, fields_json,
             present_fields_json, provenance_json, revision)
         VALUES (?1, ?2, 'work', -1, -1, ?3, ?4, ?5, ?4, ?6, 'nfo_v1', ?7, ?8,
                 '{\"kind\":\"nfo\"}', 1)
         ON CONFLICT(library_id, media_id, entity_scope, season, episode, locale, input_key)
         DO UPDATE SET
             root_id = excluded.root_id,
             path_key = excluded.path_key,
             parse_state = excluded.parse_state,
             source_version = excluded.source_version,
             fields_json = excluded.fields_json,
             present_fields_json = excluded.present_fields_json,
             provenance_json = excluded.provenance_json,
             revision = library_metadata_inputs.revision + 1",
        params![
            file.library_id.to_string(),
            file.media_id.to_string(),
            locale,
            path_key,
            file.root_id,
            parse_state,
            fields_json,
            present_fields_json,
        ],
    )?;
    Ok(())
}

/// Drop the input this file recorded after a confirmed sidecar deletion.
fn clear_nfo_input(tx: &Connection, file: &LibraryFile) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM library_metadata_inputs
         WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = 'work'
           AND input_key = ?3 AND source_version = 'nfo_v1'",
        params![
            file.library_id.to_string(),
            file.media_id.to_string(),
            file.path.to_string_lossy(),
        ],
    )?;
    Ok(())
}

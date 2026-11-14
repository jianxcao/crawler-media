use super::{MarkerResultReplacement, StoreError};
use rusqlite::params;

/// Automatic season detection cannot assign physical offsets to merged units.
/// Reject before deleting any existing facts, including an empty detection result.
pub(super) fn ensure_supported(
    tx: &rusqlite::Connection,
    replacement: &MarkerResultReplacement,
) -> Result<(), StoreError> {
    let unsupported: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_files f
         JOIN library_file_units u ON u.file_id = f.file_id
         WHERE f.media_id = ?1 AND u.season = ?2 AND f.file_id IN (SELECT value FROM json_each(?3))
           AND (SELECT COUNT(*) FROM library_file_units all_units WHERE all_units.file_id = f.file_id) > 1)",
        params![replacement.media_id.to_string(), replacement.season, serde_json::to_string(&replacement.chapter_updates.iter().map(|(id,_)| id).collect::<Vec<_>>())?],
        |row| row.get(0),
    )?;
    if unsupported {
        tracing::error!(media_id = %replacement.media_id, season = replacement.season,
            error_kind = "unit_time_mapping_unsupported", "automatic marker refresh requires unit time mapping");
        return Err(StoreError::Invalid("unit_time_mapping_unsupported: automatic multiunit marker refresh requires unit time mapping".into()));
    }
    Ok(())
}

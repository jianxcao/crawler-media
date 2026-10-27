use super::{MarkerResultReplacement, StoreError};
use rusqlite::params;
use std::collections::BTreeSet;

/// Automatic season detection cannot assign physical offsets to merged units.
/// Reject before deleting any existing facts, including an empty detection result.
pub(super) fn ensure_supported(
    tx: &rusqlite::Transaction<'_>,
    replacement: &MarkerResultReplacement,
) -> Result<(), StoreError> {
    let unsupported: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_files f
         JOIN library_file_units u ON u.file_id = f.file_id
         WHERE f.media_id = ?1 AND u.season = ?2
           AND (SELECT COUNT(*) FROM library_file_units all_units WHERE all_units.file_id = f.file_id) > 1)",
        params![replacement.media_id.to_string(), replacement.season],
        |row| row.get(0),
    )?;
    if unsupported {
        tracing::error!(media_id = %replacement.media_id, season = replacement.season,
            error_kind = "unit_time_mapping_unsupported", "automatic marker refresh requires unit time mapping");
        return Err(StoreError::Invalid("unit_time_mapping_unsupported: automatic multiunit marker refresh requires unit time mapping".into()));
    }
    Ok(())
}

/// Job publication can only replace facts within this Media and this season.
pub(super) fn covered_episodes(
    tx: &rusqlite::Transaction<'_>,
    replacement: &MarkerResultReplacement,
) -> Result<BTreeSet<i64>, StoreError> {
    let mut episodes = BTreeSet::new();
    let mut statement = tx.prepare(
        "SELECT CASE WHEN u.episode = -1 THEN 1 ELSE u.episode END
         FROM library_file_units u JOIN library_files f ON f.file_id = u.file_id
         WHERE f.file_id = ?1 AND f.media_id = ?2
           AND ((u.season = ?3 AND u.episode >= 0)
             OR (f.kind = 'movie' AND u.season = -1 AND u.episode = -1 AND ?3 = 1
                 AND (SELECT COUNT(*) FROM library_file_units all_units WHERE all_units.file_id = f.file_id) = 1))",
    )?;
    for (id, _) in &replacement.chapter_updates {
        let rows = statement.query_map(
            params![id, replacement.media_id.to_string(), replacement.season],
            |row| row.get(0),
        )?;
        for episode in rows {
            episodes.insert(episode?);
        }
    }
    Ok(episodes)
}

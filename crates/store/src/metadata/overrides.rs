use crate::{Store, StoreError};
use domain::{EntityScope, MediaId, MetadataFields};
use rusqlite::{OptionalExtension, params};

impl Store {
    pub fn set_metadata_overrides(
        &self,
        media: MediaId,
        scope: EntityScope,
        locale: &str,
        fields: &MetadataFields,
        expected_revision: i64,
    ) -> Result<i64, StoreError> {
        let (season, episode) = scope.season_episode();
        let media_str = media.to_string();
        let scope_str = scope.as_str();

        let tx = self.library.unchecked_transaction()?;

        let cur_rev: Option<i64> = tx
            .query_row(
                "SELECT revision FROM media_metadata_overrides
                 WHERE media_id = ?1 AND entity_scope = ?2 AND season = ?3 AND episode = ?4 AND locale = ?5",
                params![media_str, scope_str, season, episode, locale],
                |r| r.get(0),
            )
            .optional()?;

        let actual_rev = cur_rev.unwrap_or(0);
        if actual_rev != expected_revision {
            return Err(StoreError::Conflict(format!(
                "metadata overrides revision conflict: expected {expected_revision}, actual {actual_rev}"
            )));
        }

        let next_rev = actual_rev + 1;
        let fields_json = serde_json::to_string(fields)?;

        tx.execute(
            "INSERT INTO media_metadata_overrides
                (media_id, entity_scope, season, episode, locale, fields_json, revision)
             VALUES
                (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(media_id, entity_scope, season, episode, locale) DO UPDATE SET
                fields_json = excluded.fields_json,
                revision = excluded.revision",
            params![
                media_str,
                scope_str,
                season,
                episode,
                locale,
                fields_json,
                next_rev
            ],
        )?;

        // Overrides take part in composition: recompose the effective row and cards now.
        super::effective::write_effective(&tx, media, scope, locale)?;
        crate::library_write::reduce::reduce_media_everywhere(&tx, media)?;
        tx.commit()?;
        Ok(next_rev)
    }

    pub fn get_metadata_overrides(
        &self,
        media: MediaId,
        scope: EntityScope,
        locale: &str,
    ) -> Result<Option<(MetadataFields, i64)>, StoreError> {
        super::compose::load_overrides(&self.library, media, scope, locale)
    }
}

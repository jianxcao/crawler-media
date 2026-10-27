use domain::{Media, MediaAlias, MediaId};
use rusqlite::{OptionalExtension, params};

use crate::maps::aliases_json;
use crate::{Store, StoreError};

pub const MEDIA_SELECT: &str = "
    SELECT m.media_id, m.kind, m.title, m.year, m.original_title,
           (SELECT external_id FROM media_aliases WHERE media_id = m.media_id AND source = 'tmdb' AND kind = m.kind AND is_refresh_target = 1 LIMIT 1),
           (SELECT external_id FROM media_aliases WHERE media_id = m.media_id AND source = 'douban' AND kind = m.kind AND is_refresh_target = 1 LIMIT 1),
           (SELECT external_id FROM media_aliases WHERE media_id = m.media_id AND source = 'tvdb' AND kind = m.kind AND is_refresh_target = 1 LIMIT 1),
           (SELECT external_id FROM media_aliases WHERE media_id = m.media_id AND source = 'bangumi' AND kind = m.kind AND is_refresh_target = 1 LIMIT 1),
           (SELECT external_id FROM media_aliases WHERE media_id = m.media_id AND source = 'anilist' AND kind = m.kind AND is_refresh_target = 1 LIMIT 1),
           m.aliases_display_json
    FROM media m
";

impl Store {
    /// Add an alias to a media. First alias for (source, kind) becomes the refresh target.
    /// Subsequent aliases do not silently switch the refresh target.
    pub fn add_media_alias(&self, media_id: MediaId, alias: &MediaAlias) -> Result<(), StoreError> {
        let tx = crate::write_tx(&self.library)?;
        crate::library_write::publish::ensure_media_writable(&tx, media_id)?;

        crate::library_operations::integrity_guard::ensure_media_integrity_writable(&tx, media_id)?;
        let id_str = media_id.to_string();
        // Check if this external_id is already registered to another media
        let existing: Option<String> = tx
            .query_row(
                "SELECT media_id FROM media_aliases WHERE source = ?1 AND kind = ?2 AND external_id = ?3",
                params![alias.source, alias.kind.as_str(), alias.external_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(other_id) = existing {
            if other_id != id_str {
                return Err(StoreError::Protected(format!(
                    "Alias {}:{} already registered to media {other_id}",
                    alias.source, alias.external_id
                )));
            }
            // Already registered to this media, nothing to add
            return Ok(());
        }

        // Check if there is already a refresh target for this (media_id, source, kind)
        let has_target: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM media_aliases WHERE media_id = ?1 AND source = ?2 AND kind = ?3 AND is_refresh_target = 1)",
            params![id_str, alias.source, alias.kind.as_str()],
            |r| r.get(0),
        )?;

        let is_target = if has_target { 0 } else { 1 };
        tx.execute(
            "INSERT INTO media_aliases (source, kind, external_id, media_id, is_refresh_target)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                alias.source,
                alias.kind.as_str(),
                alias.external_id,
                id_str,
                is_target
            ],
        )?;

        // If media was provisional, it's now confirmed
        tx.execute(
            "UPDATE media SET identity_state = 'confirmed' WHERE media_id = ?1 AND identity_state = 'provisional'",
            params![id_str],
        )?;
        tx.execute(
            "UPDATE library_items SET identity_state = 'confirmed' WHERE media_id = ?1 AND identity_state = 'provisional'",
            params![id_str],
        )?;

        // Rebuild cards everywhere for affected media
        crate::library_write::reduce::reduce_media_everywhere(&tx, media_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Read the current refresh target alias for a given source, restricted to the
    /// Media's own kind. `media_aliases(media_id, kind)` references
    /// `media(media_id, kind)`, so this matches the `kind = m.kind` filter of
    /// `MEDIA_SELECT`: the flat provider ids a reader sees and the id a refresh
    /// requests are the same alias. `alias_target` is unique per
    /// `(media_id, source, kind)`, and `ORDER BY external_id` pins the pick, so the
    /// request is deterministic.
    pub fn media_refresh_alias(
        &self,
        media: MediaId,
        source: &str,
    ) -> Result<Option<MediaAlias>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT a.source, a.kind, a.external_id FROM media_aliases a
             JOIN media m ON m.media_id = a.media_id AND m.kind = a.kind
             WHERE a.media_id = ?1 AND a.source = ?2 AND a.is_refresh_target = 1
             ORDER BY a.external_id ASC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![media.to_string(), source])?;
        if let Some(row) = rows.next()? {
            let source: String = row.get(0)?;
            let kind_str: String = row.get(1)?;
            let kind = kind_str.parse().map_err(StoreError::MediaKind)?;
            let external_id: String = row.get(2)?;
            Ok(Some(MediaAlias {
                source,
                kind,
                external_id,
            }))
        } else {
            Ok(None)
        }
    }

    /// Transactional update of media scalar identity with revision CAS and writable checks.
    /// Rebuilds derived library summaries everywhere for the affected media.
    pub fn update_media_identity(
        &self,
        expected_revision: i64,
        media: &Media,
    ) -> Result<i64, StoreError> {
        let tx = crate::write_tx(&self.library)?;
        let id_str = media.id.to_string();

        let current_state_and_rev: Option<(String, i64)> = tx
            .query_row(
                "SELECT identity_state, metadata_revision FROM media WHERE media_id = ?1",
                params![id_str],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        let Some((state, cur_rev)) = current_state_and_rev else {
            return Err(StoreError::Missing(format!("media {}", media.id)));
        };

        if state == "merging" || state == "retired" {
            return Err(StoreError::Protected(format!(
                "media {} identity is frozen ({state})",
                media.id
            )));
        }

        if cur_rev != expected_revision {
            return Err(StoreError::Protected(format!(
                "media {} metadata_revision mismatch: expected {}, got {}",
                media.id, expected_revision, cur_rev
            )));
        }

        let next_rev = cur_rev + 1;
        tx.execute(
            "UPDATE media SET title = ?2, year = ?3, original_title = ?4, aliases_display_json = ?5, metadata_revision = ?6 WHERE media_id = ?1",
            params![
                id_str,
                media.title,
                media.year.map(i64::from),
                media.original_title,
                aliases_json(&media.aliases),
                next_rev,
            ],
        )?;

        // Rebuild cards everywhere
        crate::library_write::reduce::reduce_media_everywhere(&tx, media.id)?;
        tx.commit()?;
        Ok(next_rev)
    }

    /// Read the current metadata_revision of a media.
    pub fn media_revision(&self, media_id: MediaId) -> Result<Option<i64>, StoreError> {
        let sql = "SELECT metadata_revision FROM media WHERE media_id = ?1";
        self.library
            .query_row(sql, params![media_id.to_string()], |r| r.get(0))
            .optional()
            .map_err(Into::into)
    }
}

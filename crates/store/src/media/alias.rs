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
        self.with_library_write(|db| db.add_media_alias(media_id, alias))
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
        self.with_library_read(|db| db.media_refresh_alias(media, source))
    }

    /// Transactional update of media scalar identity with revision CAS and writable checks.
    /// Rebuilds derived library summaries everywhere for the affected media.
    pub fn update_media_identity(
        &self,
        expected_revision: i64,
        media: &Media,
    ) -> Result<i64, StoreError> {
        self.with_library_write(|db| db.update_media_identity(expected_revision, media))
    }

    /// Read the current metadata_revision of a media.
    pub fn media_revision(&self, media_id: MediaId) -> Result<Option<i64>, StoreError> {
        self.with_library_read(|db| db.media_revision(media_id))
    }
}

#[path = "alias/connection.rs"]
mod connection;

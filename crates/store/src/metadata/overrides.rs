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
        self.with_library_write(|db| {
            db.set_metadata_overrides(media, scope, locale, fields, expected_revision)
        })
    }

    pub fn get_metadata_overrides(
        &self,
        media: MediaId,
        scope: EntityScope,
        locale: &str,
    ) -> Result<Option<(MetadataFields, i64)>, StoreError> {
        self.with_library_read(|db| db.get_metadata_overrides(media, scope, locale))
    }
}

#[path = "overrides/connection.rs"]
mod connection;

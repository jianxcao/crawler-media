//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn effective_metadata_fields(
        &self,
        library: LibraryId,
        media: MediaId,
        scope: EntityScope,
        locale: &str,
    ) -> Result<ComposedMetadata, StoreError> {
        let actual_locale = self
            .resolved_metadata_locale(library, media, locale)?
            .unwrap_or_else(|| locale.to_string());
        let (fields, field_origins) = compose_entity(self.library, media, scope, &actual_locale)?;
        Ok(ComposedMetadata {
            fields,
            actual_locale,
            field_origins,
        })
    }
}

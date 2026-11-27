//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn publish_file_checked_context(
        &self,
        scope: &WriteScope,
        publication: &FilePublication<'_>,
        expected: Option<&LibraryFile>,
        now_ms: i64,
    ) -> Result<PublishReport, StoreError> {
        self.publish_files_with_validation(scope, std::slice::from_ref(publication), now_ms, |tx| {
            validate_context(tx, publication.file, expected)
        })
    }

    pub fn publish_unidentified_checked_context(
        &self,
        scope: &WriteScope,
        publication: &FilePublication<'_>,
        expected: Option<&LibraryFile>,
        source: &crate::UnidentifiedRow,
        now_ms: i64,
    ) -> Result<PublishReport, StoreError> {
        self.publish_unidentified_checked_context_with_outbox(
            scope,
            publication,
            expected,
            source,
            now_ms,
            None,
        )
    }

    pub fn publish_unidentified_checked_context_with_outbox(
        &self,
        scope: &WriteScope,
        publication: &FilePublication<'_>,
        expected: Option<&LibraryFile>,
        source: &crate::UnidentifiedRow,
        now_ms: i64,
        outbox: Option<&crate::plugin_events::EmitOutboxEventInput>,
    ) -> Result<PublishReport, StoreError> {
        let slice = outbox.map(std::slice::from_ref).unwrap_or(&[]);
        self.publish_files_with_validation_and_outbox(
            scope,
            std::slice::from_ref(publication),
            now_ms,
            |tx| {
                validate_context(tx, publication.file, expected)?;
                crate::unidentified::consume(tx, source)
            },
            slice,
        )
    }
}

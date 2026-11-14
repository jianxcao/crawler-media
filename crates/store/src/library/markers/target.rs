//! Validate the pinned target, including a confirmed empty result, inside its
//! publication transaction. No filesystem observation belongs in this module.
use super::{SourceObservation, StoreError, source_guard};
use domain::{FileCoverage, FilePresence, FilePublication};

impl crate::LibraryConnection<'_> {
    pub(super) fn validate_marker_target(
        &self,
        target: &FileCoverage,
        members: Option<&[SourceObservation]>,
    ) -> Result<(), StoreError> {
        let file = &target.file;
        crate::library_write::publish_checked::validate_context(self.library, file, Some(file))?;
        let current = self
            .file_coverage_by_id(file.file_id)?
            .ok_or_else(|| StoreError::Invalid("marker_source_changed".into()))?;
        let mut units = target.units.clone();
        units.sort_by_key(|unit| (unit.season, unit.episode));
        if current.units != units
            || current.file.presence != FilePresence::Present
            || current.file.publication != FilePublication::Published
        {
            return Err(StoreError::Invalid(
                "marker_source_changed: target coverage changed".into(),
            ));
        }
        if let Some(members) = members {
            source_guard::check_observed(self.library, &file.file_id.to_string(), members)?;
        }
        crate::library_write::ensure_library_writable(self.library, &file.library_id.to_string())?;
        crate::library_write::ensure_media_writable(self.library, file.media_id)?;
        let owner = crate::library_key_owner::owning_root_key(self.library, &file.path, file.kind)?;
        if owner != Some((file.library_id.to_string(), file.root_id.clone())) {
            return Err(StoreError::Protected(
                "marker target ownership changed".into(),
            ));
        }
        Ok(())
    }
}

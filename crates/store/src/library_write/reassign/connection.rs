//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn reassign_files(
        &self,
        request: &ReassignFiles,
        now_ms: i64,
    ) -> Result<usize, StoreError> {
        validate_request(request)?;
        let tx = crate::write_tx(self.library)?;
        ensure_target_accepts_reassign(&tx, request.to)?;
        super::super::publish::ensure_media_writable(&tx, request.from)?;
        super::super::publish::ensure_media_writable(&tx, request.to)?;
        ensure_source_revision(&tx, request.from, request.expected_media_revision)?;
        let target_kind: String = media_kind(&tx, request.to)?;
        let mut libraries = BTreeSet::new();
        for (file_id, version) in &request.files {
            let library = ensure_file_movable(&tx, file_id, version, request.from, &target_kind)?;
            libraries.insert(library);
        }
        let ended = terminate_bound_playback(&tx, &request.files)?;
        for (file_id, _) in &request.files {
            move_file_ownership(&tx, *file_id, request.to)?;
        }
        for library in &libraries {
            let library_id = library.parse::<domain::LibraryId>()?;
            reduce_library_media(&tx, library_id, request.from)?;
            reduce_library_media(&tx, library_id, request.to)?;
        }
        tx.commit()?;
        tracing::info!(
            from = %request.from,
            to = %request.to,
            moved = request.files.len(),
            libraries = libraries.len(),
            sessions_ended = ended,
            now_ms,
            "reassigned explicit files to another Media",
        );
        Ok(request.files.len())
    }
}

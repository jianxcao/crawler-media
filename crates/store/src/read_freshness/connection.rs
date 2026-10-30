//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn read_fingerprint(
        &self,
        library_id: &str,
        media_id: MediaId,
    ) -> Result<ReadFingerprint, StoreError> {
        let config_revision = self.library_config_revision(library_id)?;
        let mut stmt = self.library.prepare(
            "SELECT f.file_id, f.file_revision, f.presence_state, f.publication_state,
                    f.root_id, f.display_path, f.source_version, f.resolution, f.codec,
                    f.hdr, f.quality_source, f.confidence, f.filter_score, f.release_quality,
                    f.hdr_observed,
                    u.season, u.episode
             FROM library_files f
             LEFT JOIN library_file_units u ON u.file_id = f.file_id
             WHERE f.library_id = ?1 AND f.media_id = ?2
             ORDER BY f.file_id, u.season, u.episode",
        )?;
        let mut rows = stmt.query(params![library_id, media_id.to_string()])?;
        let mut files: Vec<FileReadRevision> = Vec::new();
        while let Some(row) = rows.next()? {
            let file_id: String = row.get(0)?;
            if files.last().is_none_or(|file| file.file_id != file_id) {
                files.push(FileReadRevision {
                    file_id,
                    file_revision: row.get(1)?,
                    presence_state: row.get(2)?,
                    publication_state: row.get(3)?,
                    root_id: row.get(4)?,
                    path: row.get(5)?,
                    source_version: row.get(6)?,
                    resolution: row.get(7)?,
                    codec: row.get(8)?,
                    hdr: row.get(9)?,
                    quality_source: row.get(10)?,
                    confidence: row.get(11)?,
                    filter_score: row.get(12)?,
                    release_quality: row.get(13)?,
                    hdr_observed: row.get(14)?,
                    units: Vec::new(),
                });
            }
            if let (Some(season), Some(episode)) = (row.get(15)?, row.get(16)?)
                && let Some(file) = files.last_mut()
            {
                file.units.push(UnitKey::new(season, episode));
            }
        }
        tracing::debug!(
            library_id,
            media_id = %media_id,
            config_revision,
            file_count = files.len(),
            "captured scoped read fingerprint"
        );
        Ok(ReadFingerprint {
            library_id: library_id.to_string(),
            media_id,
            config_revision,
            files,
        })
    }
}

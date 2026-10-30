//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn publish_files(
        &self,
        scope: &WriteScope,
        files: &[FilePublication<'_>],
        now_ms: i64,
    ) -> Result<PublishReport, StoreError> {
        self.publish_files_with_validation(scope, files, now_ms, |_| Ok(()))
    }

    pub(in crate::library_write) fn publish_files_with_validation(
        &self,
        scope: &WriteScope,
        files: &[FilePublication<'_>],
        now_ms: i64,
        validate: impl FnOnce(&Connection) -> Result<(), StoreError>,
    ) -> Result<PublishReport, StoreError> {
        self.publish_files_counted(
            scope,
            files,
            now_ms,
            validate,
            &mut PublicationCounters::default(),
        )
    }

    #[doc(hidden)]
    #[cfg(feature = "test-support")]
    pub fn publish_files_with_stats(
        &self,
        scope: &WriteScope,
        files: &[FilePublication<'_>],
        now_ms: i64,
    ) -> Result<(PublishReport, PublishStats), StoreError> {
        let mut counters = PublicationCounters::default();
        let written_before = self.library.total_changes();
        let report = self.publish_files_counted(scope, files, now_ms, |_| Ok(()), &mut counters)?;
        let mut stats = counters.into_stats();
        stats.written_rows = self.library.total_changes() - written_before;
        Ok((report, stats))
    }

    pub(super) fn publish_files_counted(
        &self,
        scope: &WriteScope,
        files: &[FilePublication<'_>],
        now_ms: i64,
        validate: impl FnOnce(&Connection) -> Result<(), StoreError>,
        counters: &mut PublicationCounters,
    ) -> Result<PublishReport, StoreError> {
        if files.is_empty() {
            return Ok(PublishReport::default());
        }
        if files.len() > 100 {
            return Err(StoreError::Invalid(
                "exceeded 100 files per publication batch".into(),
            ));
        }
        let tx = crate::write_tx(self.library)?;
        ensure_library_writable(&tx, &scope.library_id.to_string())?;
        ensure_config_revision(&tx, scope)?;
        validate(&tx)?;
        let mut state = BatchState::default();
        for pub_item in files {
            state.publish(&tx, scope, pub_item, now_ms, counters)?;
        }
        reduce_publication_batch(
            &tx,
            scope.library_id,
            std::mem::take(&mut state.affected),
            std::mem::take(&mut state.before_projection),
            counters,
        )?;
        tx.commit()?;
        Ok(state.report)
    }

    pub fn refresh_media_summaries(
        &self,
        media_id: MediaId,
        _now_ms: i64,
    ) -> Result<usize, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let reduced = reduce_media_everywhere(&tx, media_id)?;
        tx.commit()?;
        Ok(reduced)
    }

    pub fn get_library_item(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
    ) -> Result<Option<LibraryItemSummary>, StoreError> {
        let sql = "SELECT library_id, media_id, kind, title, file_count, available_file_count,
                          missing_file_count, unknown_file_count, distinct_episode_count,
                          distinct_season_count, preferred_file_id, size_bytes
                   FROM library_items
                   WHERE library_id = ?1 AND media_id = ?2";
        let mut stmt = self.library.prepare(sql)?;
        let row = stmt
            .query_row(params![library_id.to_string(), media_id.to_string()], |r| {
                let pref_str: Option<String> = r.get(10)?;
                let pref_id = pref_str.and_then(|s| s.parse::<LedgerId>().ok());
                let size_i: Option<i64> = r.get(11)?;
                let kind_str: String = r.get(2)?;
                let kind = kind_str.parse::<MediaKind>().unwrap_or(MediaKind::Movie);

                Ok(LibraryItemSummary {
                    library_id,
                    media_id,
                    kind,
                    title: r.get(3)?,
                    file_count: r.get::<_, i64>(4)? as usize,
                    available_file_count: r.get::<_, i64>(5)? as usize,
                    missing_file_count: r.get::<_, i64>(6)? as usize,
                    unknown_file_count: r.get::<_, i64>(7)? as usize,
                    distinct_episode_count: r.get::<_, i64>(8)? as usize,
                    distinct_season_count: r.get::<_, i64>(9)? as usize,
                    preferred_file_id: pref_id,
                    size_bytes: size_i.map(|s| s as u64),
                })
            })
            .optional()?;
        Ok(row)
    }

    pub fn count_probe_obligations(&self) -> Result<usize, StoreError> {
        let count: i64 =
            self.library
                .query_row("SELECT COUNT(*) FROM file_probe_obligations", [], |r| {
                    r.get(0)
                })?;
        Ok(count as usize)
    }

    pub fn publish_file_details(
        &self,
        file_id: LedgerId,
        expected: &FileVersion,
        details: &crate::files::FileDetails,
        _now_ms: i64,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        let file_id_str = file_id.to_string();
        ensure_file_version(&tx, &file_id_str, expected)?;
        let media: String = tx.query_row(
            "SELECT media_id FROM library_files WHERE file_id=?1",
            [&file_id_str],
            |r| r.get(0),
        )?;
        ensure_media_writable(&tx, media.parse()?)?;
        upsert_file_details(&tx, &file_id_str, details)?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_file_details(
        &self,
        file_id: LedgerId,
    ) -> Result<Option<crate::files::FileDetails>, StoreError> {
        let file_id_str = file_id.to_string();
        let sql = "SELECT source_revision, source_version, tracks_version, video_json, audio_json,
                          subtitle_json, chapters_json, fingerprint_json, outro_fingerprint_json,
                          format_duration_ms
                   FROM file_details
                   WHERE file_id = ?1";
        let mut stmt = self.library.prepare(sql)?;
        let res = stmt
            .query_row(params![file_id_str], map_file_details)
            .optional()?;
        Ok(res)
    }
}

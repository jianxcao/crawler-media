//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn published_entries_for_item(
        &self,
        item: &TypedItemId,
    ) -> Result<Vec<PlaybackSource>, StoreError> {
        let (media_id, season, episode) = match item {
            TypedItemId::Work(media) => (*media, None, None),
            TypedItemId::Season { media, season } => (*media, Some(*season as i32), None),
            TypedItemId::Unit { media, key } => (*media, Some(key.season), Some(key.episode)),
        };
        self.source_rows(None, media_id, season, episode, None, false)
    }

    pub fn published_files(
        &self,
        file_ids: &[LedgerId],
    ) -> Result<HashMap<LedgerId, LibraryFile>, StoreError> {
        let mut found = HashMap::new();
        for chunk in file_ids.chunks(SOURCE_BATCH) {
            let values: Vec<SqlValue> = chunk
                .iter()
                .map(|file_id| SqlValue::Text(file_id.to_string()))
                .collect();
            let sql = format!(
                "SELECT {SOURCE_COLUMNS} {SOURCE_FROM} \
                 WHERE {SOURCE_FACTS} AND f.file_id IN ({}) {SOURCE_GROUP}",
                placeholders(chunk.len(), 0)
            );
            let mut stmt = self.library.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(values), map_source)?;
            for row in rows {
                let source = row?;
                found.insert(source.file.file_id, source.file);
            }
        }
        Ok(found)
    }

    pub(crate) fn best_source_for_unit(
        &self,
        libraries: &HashSet<String>,
        media_id: MediaId,
        unit: UnitKey,
        preferred: Option<&PhysicalSourceId>,
    ) -> Result<Option<PlaybackSource>, StoreError> {
        self.source_rows(
            Some(libraries),
            media_id,
            Some(unit.season),
            Some(unit.episode),
            preferred,
            false,
        )
        .map(|sources| sources.into_iter().next())
    }

    pub(crate) fn file_tracks_version(&self, file_id: LedgerId) -> Result<Option<i64>, StoreError> {
        self.library
            .query_row(
                "SELECT COALESCE(d.tracks_version, f.tracks_version)
                 FROM library_files f
                 LEFT JOIN file_details d ON d.file_id = f.file_id
                 WHERE f.file_id = ?1
                   AND f.presence_state = 'present' AND f.publication_state = 'published'",
                rusqlite::params![file_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub(super) fn source_rows(
        &self,
        libraries: Option<&HashSet<String>>,
        media_id: MediaId,
        season: Option<i32>,
        episode: Option<i32>,
        preferred: Option<&PhysicalSourceId>,
        limited: bool,
    ) -> Result<Vec<PlaybackSource>, StoreError> {
        let libraries: Vec<String> = match libraries {
            Some(set) if set.is_empty() => return Ok(Vec::new()),
            Some(set) => set.iter().cloned().collect(),
            None => Vec::new(),
        };
        let mut values = vec![SqlValue::Text(media_id.to_string())];
        let mut filters = String::from("u.media_id = ?1");
        if let Some(season) = season {
            values.push(SqlValue::Integer(season as i64));
            filters.push_str(&format!(" AND u.season = ?{}", values.len()));
        }
        if let Some(episode) = episode {
            values.push(SqlValue::Integer(episode as i64));
            filters.push_str(&format!(" AND u.episode = ?{}", values.len()));
        }
        if !libraries.is_empty() {
            filters.push_str(&format!(
                " AND f.library_id IN ({})",
                placeholders(libraries.len(), values.len())
            ));
            values.extend(library_values(&libraries));
        }
        if let Some(preferred) = preferred {
            values.push(SqlValue::Text(preferred.file_id.to_string()));
            filters.push_str(&format!(" AND f.file_id = ?{}", values.len()));
            values.push(SqlValue::Integer(preferred.revision));
            filters.push_str(&format!(" AND f.file_revision = ?{}", values.len()));
        }
        let limit = if limited { " LIMIT 1" } else { "" };
        let sql = source_sql(&filters, limit);
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values), map_source)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

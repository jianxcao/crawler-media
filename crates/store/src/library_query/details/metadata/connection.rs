//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub(crate) fn resolved_metadata_locale(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        requested: &str,
    ) -> Result<Option<String>, StoreError> {
        let mut statement = self
            .library
            .prepare("SELECT locale FROM media_metadata WHERE media_id = ?1 ORDER BY locale")?;
        let locales = statement
            .query_map([media_id.to_string()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if locales.is_empty() {
            return Ok(None);
        }
        let chain: Vec<String> = [
            Some(requested.to_string()),
            library_display_locale_on(self.library, library_id)?,
            instance_default_locale_on(self.library)?,
        ]
        .into_iter()
        .flatten()
        .collect();
        Ok(select_locale(
            &locales,
            &chain,
            LocaleFallback::AdoptedFirst,
        ))
    }

    pub fn adopted_filter_facts_for_media(
        &self,
        media_ids: &[MediaId],
        preferred_locale: Option<&str>,
    ) -> Result<HashMap<MediaId, AdoptedFilterFacts>, StoreError> {
        if media_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut by_media: HashMap<MediaId, Vec<(String, String, String, String)>> = HashMap::new();
        for chunk in media_ids.chunks(500) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT media_id, locale, genres_json, countries_json, details_json
                 FROM media_metadata
                 WHERE media_id IN ({placeholders})
                 ORDER BY media_id, locale"
            );
            let mut statement = self.library.prepare(&sql)?;
            let mut rows = statement.query(rusqlite::params_from_iter(
                chunk.iter().map(|id| id.to_string()),
            ))?;
            while let Some(row) = rows.next()? {
                let id_raw: String = row.get(0)?;
                let Ok(media_id) = id_raw.parse::<MediaId>() else {
                    continue;
                };
                let locale: String = row.get(1)?;
                let genres_json: String = row.get(2)?;
                let countries_json: String = row.get(3)?;
                let details_json: String = row.get(4)?;
                by_media.entry(media_id).or_default().push((
                    locale,
                    genres_json,
                    countries_json,
                    details_json,
                ));
            }
        }

        let mut out = HashMap::with_capacity(by_media.len());
        for (media_id, rows) in by_media {
            if let Some((_, genres_json, countries_json, details_json)) =
                pick_locale_row(&rows, preferred_locale)
                && let Some(facts) =
                    adopted_facts_from_json(genres_json, countries_json, details_json, media_id)
            {
                out.insert(media_id, facts);
            }
        }
        Ok(out)
    }

    pub fn adopted_filter_facts(
        &self,
        media_ids: &[MediaId],
        preferred_locale: &str,
    ) -> Result<HashMap<MediaId, AdoptedFilterFacts>, StoreError> {
        self.adopted_filter_facts_for_media(media_ids, Some(preferred_locale))
    }
}

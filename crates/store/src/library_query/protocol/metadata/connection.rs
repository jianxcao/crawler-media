//! SQL on one explicitly borrowed database connection.
use super::*;
use crate::library_query::{instance_default_locale_on, library_display_locale_on};

impl crate::LibraryConnection<'_> {
    pub fn protocol_page_metadata(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        scope: EntityScope,
        locale: Option<&str>,
    ) -> Result<MetadataFields, StoreError> {
        let mut fields = self.scoped_page_fields(library_id, media_id, scope, locale)?;
        if !matches!(scope, EntityScope::Work) {
            let work = self.scoped_page_fields(library_id, media_id, EntityScope::Work, locale)?;
            overlay_missing(&mut fields, &work);
        }
        Ok(fields)
    }

    pub(super) fn scoped_page_fields(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        scope: EntityScope,
        locale: Option<&str>,
    ) -> Result<MetadataFields, StoreError> {
        let mut fields = self.page_observed_fields(library_id, media_id, scope, locale)?;
        let Some(resolved) =
            self.resolved_metadata_locale(library_id, media_id, locale.unwrap_or(""))?
        else {
            return Ok(fields);
        };
        if matches!(scope, EntityScope::Work)
            && let Some(adopted) = self.get_effective_metadata(media_id, &resolved)?
        {
            overlay_adopted_details(&mut fields, &adopted.details_json, media_id);
            fill_from_adopted(&mut fields, &adopted);
        }
        let composed = self.effective_metadata_fields(library_id, media_id, scope, &resolved)?;
        overlay_missing(&mut fields, &composed.fields);
        Ok(fields)
    }

    pub(super) fn page_observed_fields(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        scope: EntityScope,
        locale: Option<&str>,
    ) -> Result<MetadataFields, StoreError> {
        if matches!(scope, EntityScope::Work) {
            // work 作用域复用 Web 浏览同一条读路径，避免两套 locale 语义。
            return self.metadata_input_fields(library_id, media_id, locale);
        }
        let Some(chosen) = self.scoped_input_locale(library_id, media_id, scope, locale)? else {
            return Ok(MetadataFields::default());
        };
        let (season, episode) = scope.season_episode();
        let fields_json: Option<String> = self
            .library
            .query_row(
                "SELECT fields_json FROM library_metadata_inputs
                 WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = ?3
                   AND season = ?4 AND episode = ?5
                   AND parse_state = 'valid' AND locale = ?6
                 ORDER BY input_key
                 LIMIT 1",
                params![
                    library_id.to_string(),
                    media_id.to_string(),
                    scope.as_str(),
                    season,
                    episode,
                    chosen
                ],
                |row| row.get(0),
            )
            .optional()?;
        match fields_json {
            Some(fields_json) => Ok(serde_json::from_str(&fields_json)?),
            None => Ok(MetadataFields::default()),
        }
    }

    pub(super) fn scoped_input_locale(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        scope: EntityScope,
        requested: Option<&str>,
    ) -> Result<Option<String>, StoreError> {
        let (season, episode) = scope.season_episode();
        let mut statement = self.library.prepare(
            "SELECT DISTINCT locale FROM library_metadata_inputs
             WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = ?3
               AND season = ?4 AND episode = ?5 AND parse_state = 'valid'
             ORDER BY locale",
        )?;
        let locales = statement
            .query_map(
                params![
                    library_id.to_string(),
                    media_id.to_string(),
                    scope.as_str(),
                    season,
                    episode
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        if locales.is_empty() {
            return Ok(None);
        }
        let chain = [
            requested.map(str::to_string),
            library_display_locale_on(self.library, library_id)?,
            instance_default_locale_on(self.library)?,
        ];
        for candidate in chain.into_iter().flatten() {
            let candidate = candidate.trim();
            if candidate.is_empty() {
                continue;
            }
            if let Some(exact) = locales.iter().find(|stored| stored.as_str() == candidate) {
                return Ok(Some(exact.clone()));
            }
            let prefix = candidate.split(['-', '_']).next().unwrap_or(candidate);
            if let Some(prefix_match) = locales
                .iter()
                .find(|stored| stored.split(['-', '_']).next().unwrap_or(stored) == prefix)
            {
                return Ok(Some(prefix_match.clone()));
            }
        }
        Ok(locales.into_iter().find(|stored| stored.is_empty()))
    }
}

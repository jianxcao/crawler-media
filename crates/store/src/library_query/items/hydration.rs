use std::collections::HashMap;

use domain::{LibraryId, MediaId, UserId};
use rusqlite::params_from_iter;

use super::query::Bindings;
use crate::{
    Store, StoreError,
    library_query::types::{LibraryItemFacts, LibraryStill},
};

impl Store {
    /// Page-scoped card facts: seasons and studio metadata for the page's media ids, one
    /// query each. Favourites come from `user_library_items` when a user is signed in.
    pub fn library_item_facts(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
        user_id: Option<UserId>,
    ) -> Result<HashMap<MediaId, LibraryItemFacts>, StoreError> {
        let mut facts: HashMap<MediaId, LibraryItemFacts> = media_ids
            .iter()
            .map(|media_id| (*media_id, LibraryItemFacts::default()))
            .collect();
        if media_ids.is_empty() {
            return Ok(facts);
        }
        for media_chunk in media_ids.chunks(crate::browse_reads::MEDIA_BATCH_CHUNK) {
            self.fill_seasons(library_id, media_chunk, &mut facts)?;
            self.fill_studio(library_id, media_chunk, &mut facts)?;
            if let Some(user_id) = user_id {
                self.fill_favorites(library_id, media_chunk, user_id, &mut facts)?;
            }
        }
        Ok(facts)
    }

    fn fill_seasons(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
        facts: &mut HashMap<MediaId, LibraryItemFacts>,
    ) -> Result<(), StoreError> {
        let mut bindings = Bindings::new();
        let library = bindings.push(library_id.to_string().into());
        let list =
            bindings.placeholders(media_ids.len(), |index| media_ids[index].to_string().into());
        let sql = format!(
            "SELECT media_id, season FROM library_units \
             WHERE library_id = {library} AND season >= 0 AND media_id IN ({list}) \
             GROUP BY media_id, season ORDER BY media_id, season"
        );
        let mut statement = self.library.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(bindings.values()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (media_id, season) = row?;
            let Ok(media_id) = media_id.parse::<MediaId>() else {
                continue;
            };
            if let Some(facts) = facts.get_mut(&media_id) {
                facts.seasons.push(season.max(0) as u32);
            }
        }
        Ok(())
    }

    fn fill_studio(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
        facts: &mut HashMap<MediaId, LibraryItemFacts>,
    ) -> Result<(), StoreError> {
        let preferred_locale =
            crate::library_query::details::library_display_locale(self, library_id)?;
        let mut bindings = Bindings::new();
        let list =
            bindings.placeholders(media_ids.len(), |index| media_ids[index].to_string().into());
        let sql = format!(
            "SELECT m.media_id, m.locale, m.details_json, s.normalized_json \
             FROM media_metadata m \
             LEFT JOIN media_metadata_sources s \
               ON s.media_id = m.media_id AND s.entity_scope = 'work' AND s.season = -1 \
              AND s.episode = -1 AND s.locale = m.locale \
             WHERE m.media_id IN ({list}) ORDER BY m.media_id, m.locale"
        );
        let mut statement = self.library.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(bindings.values()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut picked: HashMap<MediaId, (String, Option<String>, Option<String>)> = HashMap::new();
        for row in rows {
            let (media_id, locale, details, normalized) = row?;
            let Ok(media_id) = media_id.parse::<MediaId>() else {
                continue;
            };
            let exact = preferred_locale.as_deref() == Some(locale.as_str());
            let replace = match picked.get(&media_id) {
                None => true,
                Some((kept, _, _)) => exact && preferred_locale.as_deref() != Some(kept.as_str()),
            };
            if replace {
                let (studio, logo) = studio_fields(&details, normalized.as_deref());
                picked.insert(media_id, (locale, studio, logo));
            }
        }
        for (media_id, (_, studio, logo)) in picked {
            if let Some(facts) = facts.get_mut(&media_id) {
                facts.studio = studio;
                facts.studio_logo = logo;
            }
        }
        Ok(())
    }

    fn fill_favorites(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
        user_id: UserId,
        facts: &mut HashMap<MediaId, LibraryItemFacts>,
    ) -> Result<(), StoreError> {
        let mut bindings = Bindings::new();
        let library = bindings.push(library_id.to_string().into());
        let user = bindings.push(user_id.to_string().into());
        let list =
            bindings.placeholders(media_ids.len(), |index| media_ids[index].to_string().into());
        let sql = format!(
            "SELECT media_id, favorite FROM user_library_items \
             WHERE library_id = {library} AND user_id = {user} AND media_id IN ({list})"
        );
        let mut statement = self.library.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(bindings.values()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (media_id, favorite) = row?;
            let Ok(media_id) = media_id.parse::<MediaId>() else {
                continue;
            };
            if let Some(facts) = facts.get_mut(&media_id) {
                facts.favorite = favorite != 0;
            }
        }
        Ok(())
    }

    /// Stills of the page's media ids: published files whose `has_still` is set, with the
    /// unit each file covers. One query for the whole page.
    pub fn library_item_stills(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
    ) -> Result<Vec<LibraryStill>, StoreError> {
        let mut stills = Vec::new();
        for media_chunk in media_ids.chunks(crate::browse_reads::MEDIA_BATCH_CHUNK) {
            self.read_stills_batch(library_id, media_chunk, &mut stills)?;
        }
        Ok(stills)
    }

    fn read_stills_batch(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
        stills: &mut Vec<LibraryStill>,
    ) -> Result<(), StoreError> {
        let mut bindings = Bindings::new();
        let library = bindings.push(library_id.to_string().into());
        let list =
            bindings.placeholders(media_ids.len(), |index| media_ids[index].to_string().into());
        let sql = format!(
            "SELECT f.media_id, f.file_id, u.season, u.episode \
             FROM library_files f \
             JOIN library_file_units u ON u.file_id = f.file_id AND u.library_id = f.library_id \
             WHERE f.library_id = {library} AND f.publication_state = 'published' \
               AND f.has_still = 1 AND f.media_id IN ({list}) \
             ORDER BY f.media_id, u.season, u.episode, f.file_id"
        );
        let mut statement = self.library.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(bindings.values()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (media, file, season, episode) = row?;
            let (Ok(media_id), Ok(file_id)) = (media.parse::<MediaId>(), file.parse()) else {
                continue;
            };
            stills.push(LibraryStill {
                media_id,
                file_id,
                season: (season >= 0).then_some(season as u32),
                episode: (episode >= 0).then_some(episode as u32),
            });
        }
        Ok(())
    }
}

/// Studio name and logo of an item: the adopted row's `details_json` first, then the adopted
/// source payload. `None` when the metadata carries neither.
fn studio_fields(
    details_json: &str,
    normalized_json: Option<&str>,
) -> (Option<String>, Option<String>) {
    let details: serde_json::Value = serde_json::from_str(details_json).unwrap_or_default();
    let studio = details
        .get("studio")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| first_string(details.get("studios")));
    let logo = details
        .get("studio_logo")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    if studio.is_some() || logo.is_some() {
        return (studio, logo);
    }
    let Some(normalized) = normalized_json else {
        return (None, None);
    };
    let fields: serde_json::Value = serde_json::from_str(normalized).unwrap_or_default();
    let fields = fields.get("fields").cloned().unwrap_or(fields);
    let studio = first_string(fields.get("studios"));
    let logo = fields
        .get("studio_logo")
        .and_then(|value| value.get("value"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    (studio, logo)
}

fn first_string(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::Array(values) => values
            .first()
            .and_then(|value| value.as_str())
            .map(str::to_string),
        serde_json::Value::String(value) => Some(value.clone()),
        _ => None,
    }
}

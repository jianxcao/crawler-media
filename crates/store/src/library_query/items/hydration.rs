use std::collections::HashMap;

use domain::{LibraryId, MediaId, UserId};
use rusqlite::{Connection, OptionalExtension, params_from_iter};

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
        library_item_facts_on(&self.library, library_id, media_ids, user_id)
    }

    /// Stills of the page's media ids: published files whose `has_still` is set, with the
    /// unit each file covers. One query for the whole page.
    pub fn library_item_stills(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
    ) -> Result<Vec<LibraryStill>, StoreError> {
        library_item_stills_on(&self.library, library_id, media_ids)
    }
}

pub(crate) fn library_item_facts_on(
    conn: &Connection,
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
    for media_chunk in media_ids.chunks(crate::file_reads::MEDIA_BATCH_CHUNK) {
        fill_seasons(conn, library_id, media_chunk, &mut facts)?;
        fill_studio(conn, library_id, media_chunk, &mut facts)?;
        if let Some(user_id) = user_id {
            fill_favorites(conn, library_id, media_chunk, user_id, &mut facts)?;
        }
    }
    Ok(facts)
}

fn fill_seasons(
    conn: &Connection,
    library_id: LibraryId,
    media_ids: &[MediaId],
    facts: &mut HashMap<MediaId, LibraryItemFacts>,
) -> Result<(), StoreError> {
    let mut bindings = Bindings::new();
    let library = bindings.push(library_id.to_string().into());
    let list = bindings.placeholders(media_ids.len(), |index| media_ids[index].to_string().into());
    let sql = format!(
        "SELECT media_id, season FROM library_units \
             WHERE library_id = {library} AND season >= 0 AND media_id IN ({list}) \
             GROUP BY media_id, season ORDER BY media_id, season"
    );
    let mut statement = conn.prepare(&sql)?;
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
    conn: &Connection,
    library_id: LibraryId,
    media_ids: &[MediaId],
    facts: &mut HashMap<MediaId, LibraryItemFacts>,
) -> Result<(), StoreError> {
    let preferred_locale = library_display_locale_on(conn, library_id)?;
    let mut bindings = Bindings::new();
    let list = bindings.placeholders(media_ids.len(), |index| media_ids[index].to_string().into());
    // Light projection: only the four studio strings leave SQLite, never the
    // multi-KB `details_json` / `normalized_json` blobs (Q3a benchmark).
    let d_studio = format!(
        "COALESCE({}, {})",
        json_text("m.details_json", "$.studio"),
        json_first("m.details_json", "$.studios")
    );
    let d_logo = json_text("m.details_json", "$.studio_logo");
    let n_studio = format!(
        "CASE WHEN json_type(s.normalized_json, '$.fields') IS NOT NULL THEN {} ELSE {} END",
        json_first("s.normalized_json", "$.fields.studios"),
        json_first("s.normalized_json", "$.studios")
    );
    let n_logo = format!(
        "CASE WHEN json_type(s.normalized_json, '$.fields') IS NOT NULL THEN {} ELSE {} END",
        json_text("s.normalized_json", "$.fields.studio_logo.value"),
        json_text("s.normalized_json", "$.studio_logo.value")
    );
    let sql = format!(
        "SELECT m.media_id, m.locale, {d_studio}, {d_logo}, {n_studio}, {n_logo} \
             FROM media_metadata m \
             LEFT JOIN media_metadata_sources s \
               ON s.media_id = m.media_id AND s.entity_scope = 'work' AND s.season = -1 \
              AND s.episode = -1 AND s.locale = m.locale \
             WHERE m.media_id IN ({list}) ORDER BY m.media_id, m.locale"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(bindings.values()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            [
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ],
        ))
    })?;
    let mut picked: HashMap<MediaId, (String, Option<String>, Option<String>)> = HashMap::new();
    for row in rows {
        let (media_id, locale, [d_studio, d_logo, n_studio, n_logo]) = row?;
        let Ok(media_id) = media_id.parse::<MediaId>() else {
            continue;
        };
        let exact = preferred_locale.as_deref() == Some(locale.as_str());
        let replace = match picked.get(&media_id) {
            None => true,
            Some((kept, _, _)) => exact && preferred_locale.as_deref() != Some(kept.as_str()),
        };
        if replace {
            let (studio, logo) = if d_studio.is_some() || d_logo.is_some() {
                (d_studio, d_logo)
            } else {
                (n_studio, n_logo)
            };
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
    conn: &Connection,
    library_id: LibraryId,
    media_ids: &[MediaId],
    user_id: UserId,
    facts: &mut HashMap<MediaId, LibraryItemFacts>,
) -> Result<(), StoreError> {
    let mut bindings = Bindings::new();
    let library = bindings.push(library_id.to_string().into());
    let user = bindings.push(user_id.to_string().into());
    let list = bindings.placeholders(media_ids.len(), |index| media_ids[index].to_string().into());
    let sql = format!(
        "SELECT media_id, favorite FROM user_library_items \
             WHERE library_id = {library} AND user_id = {user} AND media_id IN ({list})"
    );
    let mut statement = conn.prepare(&sql)?;
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

pub(crate) fn library_item_stills_on(
    conn: &Connection,
    library_id: LibraryId,
    media_ids: &[MediaId],
) -> Result<Vec<LibraryStill>, StoreError> {
    let mut stills = Vec::new();
    for media_chunk in media_ids.chunks(crate::file_reads::MEDIA_BATCH_CHUNK) {
        read_stills_batch(conn, library_id, media_chunk, &mut stills)?;
    }
    Ok(stills)
}

fn read_stills_batch(
    conn: &Connection,
    library_id: LibraryId,
    media_ids: &[MediaId],
    stills: &mut Vec<LibraryStill>,
) -> Result<(), StoreError> {
    let mut bindings = Bindings::new();
    let library = bindings.push(library_id.to_string().into());
    let list = bindings.placeholders(media_ids.len(), |index| media_ids[index].to_string().into());
    let sql = format!(
        "SELECT f.media_id, f.file_id, u.season, u.episode \
             FROM library_files f \
             JOIN library_file_units u ON u.file_id = f.file_id AND u.library_id = f.library_id \
             WHERE f.library_id = {library} AND f.publication_state = 'published' \
               AND f.has_still = 1 AND f.media_id IN ({list}) \
             ORDER BY f.media_id, u.season, u.episode, f.file_id"
    );
    let mut statement = conn.prepare(&sql)?;
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

/// Studio name and logo of an item: the adopted row's `details_json` first, then the adopted
/// source payload. `None` when the metadata carries neither.
/// SQL for "the JSON text at `path`, else NULL" (non-string values read as absent).
fn json_text(column: &str, path: &str) -> String {
    format!(
        "CASE WHEN json_type({column}, '{path}') = 'text' THEN json_extract({column}, '{path}') END"
    )
}

/// SQL for "a string at `path`, or the first element when it is an array".
fn json_first(column: &str, path: &str) -> String {
    format!(
        "COALESCE({}, CASE WHEN json_type({column}, '{path}') = 'array' THEN {} END)",
        json_text(column, path),
        json_text(column, &format!("{path}[0]"))
    )
}

fn library_display_locale_on(
    conn: &Connection,
    library_id: LibraryId,
) -> Result<Option<String>, StoreError> {
    let display: Option<String> = conn
        .query_row(
            "SELECT display_locale FROM libraries WHERE library_id=?1",
            [library_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    match display.filter(|locale| !locale.trim().is_empty()) {
        Some(locale) => Ok(Some(locale)),
        None => crate::metadata_language::default_locale(conn),
    }
}

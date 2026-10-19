//! Item detail, unit pages and episode lists — read from the new tables only.
//!
//! Adopted metadata (`media_metadata`) is the display source of an item; the fields a Scrape
//! wrote (`library_metadata_inputs`) fill the gaps. Nothing here touches the filesystem, so a
//! detail request never stats a network share.

use std::collections::BTreeMap;
use std::str::FromStr;

use domain::{FieldValue, LedgerId, LibraryId, MediaId, MetadataFields, UnitKey};
use rusqlite::{OptionalExtension, params, params_from_iter};

use super::super::{Store, StoreError};
use super::items::{Bindings, ITEM_COLUMNS, item_from_row};
use super::types::{BrowsePage, LibraryEpisode, LibraryItemDetail, LibraryUnit, UnitPage};

/// Instance-wide metadata language, `settings_keys::METADATA_LANGUAGE` of the API crate: the
/// KV key is the contract, and the store must not depend on the API crate for it.
const INSTANCE_METADATA_LANGUAGE: &str = "metadata.language";

/// Locale the instance falls back to when a library carries none.
fn instance_default_locale(store: &Store) -> Result<Option<String>, StoreError> {
    store
        .get_setting(INSTANCE_METADATA_LANGUAGE)
        .map(|value| value.filter(|locale| !locale.trim().is_empty()))
}

/// Display locale of one library, falling back to the instance default.
pub(crate) fn library_display_locale(
    store: &Store,
    library_id: LibraryId,
) -> Result<Option<String>, StoreError> {
    let display: Option<String> = store
        .library
        .query_row(
            "SELECT display_locale FROM libraries WHERE library_id = ?1",
            [library_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    match display.filter(|locale| !locale.trim().is_empty()) {
        Some(locale) => Ok(Some(locale)),
        None => instance_default_locale(store),
    }
}

impl Store {
    /// Adopted-metadata locale of one media: the requested locale first, then the library's
    /// display locale, then the instance default, then the deterministic `ORDER BY locale`.
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
        let requested = requested.trim();
        let library = library_display_locale(self, library_id)?;
        let instance = instance_default_locale(self)?;
        for candidate in [Some(requested), library.as_deref(), instance.as_deref()] {
            let Some(candidate) = candidate.filter(|value| !value.is_empty()) else {
                continue;
            };
            if let Some(found) = locales.iter().find(|locale| locale.as_str() == candidate) {
                return Ok(Some(found.clone()));
            }
        }
        Ok(locales.into_iter().next())
    }

    /// One item row, by the same column list every card query uses.
    pub(crate) fn library_item_row(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
    ) -> Result<Option<super::types::LibraryItem>, StoreError> {
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM library_items i WHERE i.library_id = ?1 AND i.media_id = ?2"
        );
        let mut statement = self.library.prepare(&sql)?;
        let row = statement
            .query_row(params![library_id.to_string(), media_id.to_string()], |row| {
                item_from_row(row)
            })
            .optional()?;
        Ok(row)
    }

    pub fn library_unit_page(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        season: Option<u32>,
        page: &BrowsePage,
    ) -> Result<UnitPage, StoreError> {
        let lib_id_str = library_id.to_string();
        let media_id_str = media_id.to_string();

        let (total_sql, rows_sql, params_vec): (String, String, Vec<rusqlite::types::Value>) =
            match season {
                Some(s) => (
                    "SELECT COUNT(*) FROM library_units WHERE library_id = ?1 AND media_id = ?2 AND season = ?3"
                        .into(),
                    "SELECT season, episode, file_count, available_file_count, preferred_file_id FROM library_units WHERE library_id = ?1 AND media_id = ?2 AND season = ?3 ORDER BY season ASC, episode ASC LIMIT ?4 OFFSET ?5"
                        .into(),
                    vec![
                        lib_id_str.clone().into(),
                        media_id_str.clone().into(),
                        (s as i64).into(),
                    ],
                ),
                None => (
                    "SELECT COUNT(*) FROM library_units WHERE library_id = ?1 AND media_id = ?2"
                        .into(),
                    "SELECT season, episode, file_count, available_file_count, preferred_file_id FROM library_units WHERE library_id = ?1 AND media_id = ?2 ORDER BY season ASC, episode ASC LIMIT ?3 OFFSET ?4"
                        .into(),
                    vec![lib_id_str.clone().into(), media_id_str.clone().into()],
                ),
            };

        let total: i64 = self.library.query_row(
            &total_sql,
            params_from_iter(params_vec.iter().cloned()),
            |r| r.get(0),
        )?;

        let (offset, limit) = match page {
            BrowsePage::Offset { start, limit } => (*start, *limit),
            BrowsePage::Cursor { limit, .. } => (0, *limit),
        };

        let mut query_params = params_vec;
        query_params.push((limit as i64).into());
        query_params.push((offset as i64).into());

        let mut stmt = self.library.prepare(&rows_sql)?;
        let rows = stmt.query_map(params_from_iter(query_params), |r| {
            let s: i32 = r.get(0)?;
            let ep: i32 = r.get(1)?;
            let fc: i64 = r.get(2)?;
            let afc: i64 = r.get(3)?;
            let pref_str: Option<String> = r.get(4)?;
            let pref_id = pref_str.and_then(|id| id.parse::<LedgerId>().ok());

            Ok(LibraryUnit {
                library_id,
                media_id,
                key: UnitKey::new(s, ep),
                file_count: fc as usize,
                available_file_count: afc as usize,
                preferred_file_id: pref_id,
            })
        })?;

        let units = rows.collect::<Result<Vec<_>, _>>()?;

        Ok(UnitPage {
            units,
            total: total as u64,
            next_cursor: None,
        })
    }

    /// Item detail: the real `library_items` columns, adopted metadata for the resolved
    /// locale (Scrape inputs filling the gaps), the first page of files with their coverage.
    pub fn library_item_detail(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        locale: &str,
    ) -> Result<Option<LibraryItemDetail>, StoreError> {
        let Some(item) = self.library_item_row(library_id, media_id)? else {
            return Ok(None);
        };
        let resolved = self.resolved_metadata_locale(library_id, media_id, locale)?;
        let mut metadata = self.metadata_input_fields(library_id, media_id, resolved.as_deref())?;
        let adopted = match resolved.as_deref() {
            Some(locale) => self.get_effective_metadata(media_id, locale)?,
            None => None,
        };
        if let Some(adopted) = &adopted {
            fill_from_adopted(&mut metadata, adopted);
        }
        let file_page = self.library_file_page(library_id, media_id, None, 50)?;
        let file_units = self.file_units(library_id, media_id, &file_page.files)?;
        Ok(Some(LibraryItemDetail {
            item,
            metadata,
            files: file_page.files,
            next_file_cursor: file_page.next_cursor,
            file_units,
        }))
    }

    /// Scrape inputs of one media: the work-scope fields the Scrape step stored, preferring
    /// the resolved locale and otherwise the deterministic `ORDER BY locale, input_key`.
    fn metadata_input_fields(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        locale: Option<&str>,
    ) -> Result<MetadataFields, StoreError> {
        let preferred = locale.map(str::to_string);
        let mut statement = self.library.prepare(
            "SELECT locale, fields_json FROM library_metadata_inputs WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = 'work' AND parse_state = 'valid' ORDER BY locale, input_key",
        )?;
        let rows = statement
            .query_map(params![library_id.to_string(), media_id.to_string()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let chosen = rows
            .iter()
            .find(|(row_locale, _)| Some(row_locale.as_str()) == preferred.as_deref())
            .or_else(|| rows.first());
        match chosen {
            Some((_, fields_json)) => Ok(serde_json::from_str(fields_json)?),
            None => Ok(MetadataFields::default()),
        }
    }

    /// Coverage of the listed files (`(-1,-1)` marks a whole-work file), one query.
    fn file_units(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        files: &[domain::LibraryFile],
    ) -> Result<Vec<(LedgerId, Vec<UnitKey>)>, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT file_id, season, episode FROM library_file_units WHERE library_id = ?1 AND media_id = ?2 ORDER BY season, episode, file_id",
        )?;
        let rows = statement.query_map(
            params![library_id.to_string(), media_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, i32>(2)?,
                ))
            },
        )?;
        let mut by_file: BTreeMap<String, Vec<UnitKey>> = BTreeMap::new();
        for row in rows {
            let (file_id, season, episode) = row?;
            by_file
                .entry(file_id)
                .or_default()
                .push(UnitKey::new(season, episode));
        }
        Ok(files
            .iter()
            .filter_map(|file| {
                by_file
                    .remove(&file.file_id.to_string())
                    .map(|units| (file.file_id, units))
            })
            .collect())
    }

    /// Episodes of one media (optionally one season): catalog metadata when adopted plus
    /// ownership from the published file facts. Replaces reading NFO files at request time.
    pub fn library_episodes(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        season: Option<u32>,
        locale: &str,
    ) -> Result<Vec<LibraryEpisode>, StoreError> {
        let resolved = self.resolved_metadata_locale(library_id, media_id, locale)?;
        let mut episodes: BTreeMap<(u32, u32), LibraryEpisode> = BTreeMap::new();
        if let Some(resolved) = resolved {
            for row in self.episode_metadata(media_id, &resolved, season)? {
                episodes.insert((row.season, row.episode), row);
            }
        }
        for (key, (file_ids, display_path, still_path)) in
            self.episode_files(library_id, media_id, season)?
        {
            let entry = episodes.entry(key).or_insert_with(|| LibraryEpisode {
                season: key.0,
                episode: key.1,
                title: None,
                overview: None,
                air_date: None,
                runtime_minutes: None,
                rating: None,
                still_url: None,
                owned: false,
                file_ids: Vec::new(),
            });
            entry.owned = true;
            // Without catalog/NFO metadata the episode name falls back to the file name.
            if entry.title.is_none() {
                entry.title = std::path::Path::new(&display_path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string);
            }
            if entry.still_url.is_none()
                && still_path.is_some()
                && let Some(file_id) = entry.file_ids.first().or_else(|| file_ids.first())
            {
                entry.still_url = Some(format!("/stills/{file_id}"));
            }
            entry.file_ids.extend(file_ids);
        }
        Ok(episodes.into_values().collect())
    }

    fn episode_metadata(
        &self,
        media_id: MediaId,
        locale: &str,
        season: Option<u32>,
    ) -> Result<Vec<LibraryEpisode>, StoreError> {
        let mut bindings = Bindings::new();
        let media = bindings.push(media_id.to_string().into());
        let locale_ph = bindings.push(locale.to_string().into());
        let mut filter = String::new();
        if let Some(season) = season {
            let season_ph = bindings.push((season as i64).into());
            filter = format!(" AND season = {season_ph}");
        }
        let sql = format!(
            "SELECT season, episode, title, overview, air_date, runtime_minutes, rating, still_url FROM media_episodes WHERE media_id = {media} AND locale = {locale_ph}{filter} ORDER BY season, episode"
        );
        let mut statement = self.library.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(bindings.values()), |row| {
            let runtime: Option<i64> = row.get(5)?;
            Ok(LibraryEpisode {
                season: row.get::<_, i64>(0)?.max(0) as u32,
                episode: row.get::<_, i64>(1)?.max(0) as u32,
                title: row.get(2)?,
                overview: row.get(3)?,
                air_date: row.get(4)?,
                runtime_minutes: runtime.map(|value| value.max(0) as u32),
                rating: row.get(6)?,
                still_url: row.get(7)?,
                owned: false,
                file_ids: Vec::new(),
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn episode_files(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        season: Option<u32>,
    ) -> Result<BTreeMap<(u32, u32), (Vec<LedgerId>, String, Option<String>)>, StoreError> {
        let mut bindings = Bindings::new();
        let library = bindings.push(library_id.to_string().into());
        let media = bindings.push(media_id.to_string().into());
        let mut filter = String::new();
        if let Some(season) = season {
            let season_ph = bindings.push((season as i64).into());
            filter = format!(" AND u.season = {season_ph}");
        }
        let sql = format!(
            "SELECT u.season, u.episode, f.file_id, f.display_path, f.still_path FROM library_file_units u JOIN library_files f ON f.file_id = u.file_id AND f.library_id = u.library_id WHERE f.library_id = {library} AND f.media_id = {media} AND f.publication_state = 'published' AND u.season >= 0 AND u.episode >= 0{filter} ORDER BY u.season, u.episode, f.file_id"
        );
        let mut statement = self.library.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(bindings.values()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut by_unit: BTreeMap<(u32, u32), (Vec<LedgerId>, String, Option<String>)> =
            BTreeMap::new();
        for row in rows {
            let (season, episode, file_id, display_path, still_path) = row?;
            let Ok(file_id) = LedgerId::from_str(&file_id) else {
                continue;
            };
            let entry = by_unit
                .entry((season.max(0) as u32, episode.max(0) as u32))
                .or_insert_with(|| (Vec::new(), display_path, still_path.clone()));
            if entry.2.is_none() {
                entry.2 = still_path;
            }
            entry.0.push(file_id);
        }
        Ok(by_unit)
    }
}

/// Fill fields the Scrape input did not carry from the adopted metadata row. An explicitly
/// empty field stays empty: only `Missing` is filled.
fn fill_from_adopted(base: &mut MetadataFields, adopted: &crate::metadata::EffectiveMetadata) {
    fill(&mut base.title, Some(adopted.title.clone()));
    fill(&mut base.original_title, adopted.original_title.clone());
    fill(&mut base.overview, adopted.overview.clone());
    fill(&mut base.release_date, adopted.release_date.clone());
    fill(&mut base.language, adopted.language.clone());
    fill(&mut base.year, adopted.year);
    fill(&mut base.rating, adopted.rating);
    fill(&mut base.runtime_minutes, adopted.runtime_minutes);
    fill(&mut base.poster_url, adopted.poster_url.clone());
    fill(&mut base.backdrop_url, adopted.backdrop_url.clone());
    if base.genres.is_missing() {
        if let Ok(genres) = serde_json::from_str::<Vec<domain::MetadataGenre>>(&adopted.genres_json)
        {
            if !genres.is_empty() {
                base.genres = FieldValue::Value(genres);
            }
        }
    }
}

fn fill<T>(field: &mut FieldValue<T>, value: Option<T>) {
    if field.is_missing() {
        if let Some(value) = value {
            *field = FieldValue::Value(value);
        }
    }
}


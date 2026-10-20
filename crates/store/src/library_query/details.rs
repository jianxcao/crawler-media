//! Item detail, unit pages and episode lists — read from the new tables only.
//!
//! Adopted metadata (`media_metadata`) is the display source of an item; the fields a Scrape
//! wrote (`library_metadata_inputs`) fill the gaps. Nothing here touches the filesystem, so a
//! detail request never stats a network share.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;

use domain::{FieldValue, LedgerId, LibraryId, MediaId, MetadataFields, UnitKey};
use rusqlite::{OptionalExtension, params, params_from_iter};

use super::super::{Store, StoreError};
use super::items::{Bindings, ITEM_COLUMNS, item_from_row};
use super::types::{BrowsePage, LibraryEpisode, LibraryItemDetail, LibraryUnit, UnitPage};

/// Adopted work metadata the browse filters agree on: genres, studios and countries.
///
/// The web library browse reads the same `media_metadata` row, so a Jellyfin genre or studio
/// filter reaches the same verdict as the web filter for the same item.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdoptedFilterFacts {
    pub genres: Vec<String>,
    pub studios: Vec<String>,
    pub countries: Vec<String>,
    pub overview: Option<String>,
    pub people: Vec<AdoptedPerson>,
}

/// One cast member of an adopted metadata row, provider-neutral.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdoptedPerson {
    pub name: String,
    pub role: Option<String>,
    pub tmdb_id: Option<String>,
}

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
pub fn library_display_locale(
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

    /// Adopted work metadata of a candidate set, one query per id batch.
    ///
    /// The requested locale wins, then the instance default, then the deterministically first
    /// locale (`ORDER BY locale`). A malformed payload is skipped with a warning so one bad row
    /// never fails a whole page; the caller's NFO fallback covers that media instead.
    pub fn adopted_filter_facts(
        &self,
        media_ids: &[MediaId],
        locale: &str,
    ) -> Result<HashMap<MediaId, AdoptedFilterFacts>, StoreError> {
        if media_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let requested = locale.trim();
        let instance = instance_default_locale(self)?;
        let mut picked: HashMap<MediaId, (bool, AdoptedFilterFacts)> = HashMap::new();
        for chunk in media_ids.chunks(crate::browse_reads::MEDIA_BATCH_CHUNK) {
            self.read_adopted_facts_batch(chunk, requested, instance.as_deref(), &mut picked)?;
        }
        Ok(picked
            .into_iter()
            .map(|(media_id, (_, facts))| (media_id, facts))
            .collect())
    }

    /// One `IN (...)` batch of [`Self::adopted_filter_facts`], merged into `picked`.
    fn read_adopted_facts_batch(
        &self,
        media_ids: &[MediaId],
        requested: &str,
        instance: Option<&str>,
        picked: &mut HashMap<MediaId, (bool, AdoptedFilterFacts)>,
    ) -> Result<(), StoreError> {
        let list = (1..=media_ids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT media_id, locale, genres_json, countries_json, details_json
             FROM media_metadata WHERE media_id IN ({list}) ORDER BY media_id, locale"
        );
        let mut statement = self.library.prepare(&sql)?;
        let mut rows =
            statement.query(params_from_iter(media_ids.iter().map(|id| id.to_string())))?;
        while let Some(row) = rows.next()? {
            let media_raw: String = row.get(0)?;
            let row_locale: String = row.get(1)?;
            let genres_json: String = row.get(2)?;
            let countries_json: String = row.get(3)?;
            let details_json: String = row.get(4)?;
            let Ok(media_id) = media_raw.parse::<MediaId>() else {
                continue;
            };
            let exact = (!requested.is_empty() && row_locale == requested)
                || instance == Some(row_locale.as_str());
            // `ORDER BY locale` makes the first row of a media the deterministic fallback; an
            // exact-locale row replaces it.
            if picked
                .get(&media_id)
                .is_some_and(|(kept_exact, _)| *kept_exact || !exact)
            {
                continue;
            }
            let Some(facts) =
                adopted_facts_from_json(&genres_json, &countries_json, &details_json, media_id)
            else {
                continue;
            };
            picked.insert(media_id, (exact, facts));
        }
        Ok(())
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

/// Parse one adopted row into filter facts. `None` means a payload is malformed: the row is
/// skipped (with a warning) so the caller can keep the next locale or its NFO fallback.
fn adopted_facts_from_json(
    genres_json: &str,
    countries_json: &str,
    details_json: &str,
    media_id: MediaId,
) -> Option<AdoptedFilterFacts> {
    let genres = parse_adopted_json(genres_json, media_id, "genres_json")?;
    let countries = parse_adopted_json(countries_json, media_id, "countries_json")?;
    let details = parse_adopted_json(details_json, media_id, "details_json")?;
    let fields = details.get("fields");
    Some(AdoptedFilterFacts {
        genres: json_names(&genres),
        studios: fields
            .and_then(|fields| fields.get("studios"))
            .map(json_names)
            .unwrap_or_default(),
        countries: json_names(&countries),
        overview: fields
            .and_then(|fields| fields.get("overview"))
            .and_then(|overview| overview.get("value"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        people: fields
            .and_then(|fields| fields.get("cast"))
            .and_then(|cast| cast.get("value"))
            .and_then(|cast| cast.as_array())
            .map(|cast| {
                cast.iter()
                    .filter_map(|member| {
                        Some(AdoptedPerson {
                            name: member.get("name")?.as_str()?.to_string(),
                            role: member
                                .get("role")
                                .and_then(|role| role.as_str())
                                .map(str::to_string),
                            tmdb_id: member
                                .get("tmdb_id")
                                .and_then(|id| id.as_str())
                                .map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn parse_adopted_json(
    raw: &str,
    media_id: MediaId,
    field: &str,
) -> Option<serde_json::Value> {
    match serde_json::from_str(raw) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(
                %error,
                %media_id,
                field,
                "skipping malformed adopted metadata while loading filter facts"
            );
            None
        }
    }
}

/// Names of a JSON payload that may be a string, a list, or a `{"name"|"value": …}` object
/// (`genres_json` holds `{"name": …}` items; `details_json.fields.studios` holds a field value).
fn json_names(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::String(text) => vec![text.clone()],
        serde_json::Value::Array(items) => items.iter().flat_map(json_names).collect(),
        serde_json::Value::Object(map) => ["name", "value"]
            .iter()
            .find_map(|key| map.get(*key))
            .map(json_names)
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}


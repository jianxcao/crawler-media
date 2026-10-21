use std::collections::{BTreeMap, HashMap};
use domain::{LedgerId, LibraryId, MediaId, UnitKey};
use rusqlite::{params, params_from_iter};
use crate::{Store, StoreError};
use crate::library_query::types::{LibraryEpisode, LibraryUnit, UnitPage};

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct UnitCursor {
    library_id: LibraryId,
    media_id: MediaId,
    season: Option<i64>,
    order: String,
    last_season: i64,
    last_episode: i64,
}

const UNIT_CURSOR_ORDER: &str = "season_episode_asc";

impl Store {
    pub fn library_unit_page(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        season: Option<u32>,
        page: &crate::library_query::types::BrowsePage,
    ) -> Result<UnitPage, StoreError> {
        let lib_id_str = library_id.to_string();
        let media_id_str = media_id.to_string();
        let scope_season = season.map(i64::from);
        let total: i64 = match scope_season {
            Some(season) => self.library.query_row(
                "SELECT COUNT(*) FROM library_units WHERE library_id = ?1 AND media_id = ?2 AND season = ?3",
                params![lib_id_str, media_id_str, season],
                |row| row.get(0),
            )?,
            None => self.library.query_row(
                "SELECT COUNT(*) FROM library_units WHERE library_id = ?1 AND media_id = ?2",
                params![lib_id_str, media_id_str],
                |row| row.get(0),
            )?,
        };

        let (sql, query_params, limit, cursor_page): (String, Vec<rusqlite::types::Value>, usize, bool) =
            match page {
                crate::library_query::types::BrowsePage::Offset { start, limit } => {
                    let mut params = vec![lib_id_str.clone().into(), media_id_str.clone().into()];
                    let where_season = if let Some(season) = scope_season {
                        params.push(season.into());
                        " AND season = ?3"
                    } else {
                        ""
                    };
                    let next = params.len() + 1;
                    params.push(i64::try_from(*limit).unwrap_or(i64::MAX).into());
                    params.push(i64::try_from(*start).unwrap_or(i64::MAX).into());
                    (format!("SELECT season, episode, file_count, available_file_count, preferred_file_id FROM library_units WHERE library_id = ?1 AND media_id = ?2{where_season} ORDER BY season ASC, episode ASC LIMIT ?{next} OFFSET ?{}", next + 1), params, *limit, false)
                }
                crate::library_query::types::BrowsePage::Cursor { after, limit } => {
                    let limit = (*limit).clamp(1, 500);
                    let mut params = vec![lib_id_str.clone().into(), media_id_str.clone().into()];
                    let mut where_sql = String::new();
                    if let Some(season) = scope_season {
                        params.push(season.into());
                        where_sql.push_str(" AND season = ?3");
                    }
                    if let Some(raw) = after {
                        let cursor: UnitCursor = serde_json::from_str(raw)
                            .map_err(|_| StoreError::Invalid("malformed unit cursor".into()))?;
                        if cursor.library_id != library_id || cursor.media_id != media_id || cursor.season != scope_season || cursor.order != UNIT_CURSOR_ORDER {
                             return Err(StoreError::Invalid("unit cursor scope mismatch".into()));
                        }
                        let season_pos = params.len() + 1;
                        params.push(cursor.last_season.into());
                        let episode_pos = params.len() + 1;
                        params.push(cursor.last_episode.into());
                        where_sql.push_str(&format!(" AND (season > ?{season_pos} OR (season = ?{season_pos} AND episode > ?{episode_pos}))"));
                    }
                    let limit_pos = params.len() + 1;
                    params.push(i64::try_from(limit + 1).unwrap_or(i64::MAX).into());
                    (format!("SELECT season, episode, file_count, available_file_count, preferred_file_id FROM library_units WHERE library_id = ?1 AND media_id = ?2{where_sql} ORDER BY season ASC, episode ASC LIMIT ?{limit_pos}"), params, limit, true)
                }
            };


        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(query_params), |row| {
            let season: i32 = row.get(0)?;
            let episode: i32 = row.get(1)?;
            let file_count: i64 = row.get(2)?;
            let available_file_count: i64 = row.get(3)?;
            let preferred = row.get::<_, Option<String>>(4)?;
            Ok(LibraryUnit {
                library_id,
                media_id,
                key: UnitKey::new(season, episode),
                file_count: file_count as usize,
                available_file_count: available_file_count as usize,
                preferred_file_id: preferred.and_then(|id| id.parse::<LedgerId>().ok()),
            })
        })?;
        let mut units = rows.collect::<Result<Vec<_>, _>>()?;
        let truncated = cursor_page && units.len() > limit;
        if truncated {
            units.pop();
        }
        let next_cursor = if truncated {
            units.last().map(|unit| serde_json::to_string(&UnitCursor {
                library_id,
                media_id,
                season: scope_season,
                order: UNIT_CURSOR_ORDER.to_string(),
                last_season: i64::from(unit.key.season),
                last_episode: i64::from(unit.key.episode),
            })).transpose().map_err(|error| StoreError::Invalid(error.to_string()))?
        } else {
            None
        };
        Ok(UnitPage { units, total: total as u64, next_cursor })
    }

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
            if entry.title.is_none() {
                entry.title = std::path::Path::new(&display_path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string);
            }
            // A served still is addressed by file id, never by the filesystem path the
            // probe recorded: the API only serves `/stills/{file_id}`.
            if entry.still_url.is_none()
                && still_path.is_some()
                && let Some(file_id) = entry.file_ids.first().or_else(|| file_ids.first())
            {
                entry.still_url = Some(format!("/stills/{file_id}"));
            }
            // One episode may have several versions: the ids accumulate, they do not
            // replace each other.
            entry.file_ids.extend(file_ids);
        }
        Ok(episodes.into_values().collect())
    }

    fn episode_metadata(
        &self,
        media_id: MediaId,
        locale: &str,
        season_filter: Option<u32>,
    ) -> Result<Vec<LibraryEpisode>, StoreError> {
        let sql = match season_filter {
            Some(_) => {
                "SELECT season, episode, title, overview, air_date, runtime_minutes, rating, still_url
                 FROM media_episodes WHERE media_id = ?1 AND locale = ?2 AND season = ?3
                 ORDER BY season, episode"
            }
            None => {
                "SELECT season, episode, title, overview, air_date, runtime_minutes, rating, still_url
                 FROM media_episodes WHERE media_id = ?1 AND locale = ?2
                 ORDER BY season, episode"
            }
        };
        let mut statement = self.library.prepare(sql)?;
        let rows = match season_filter {
            Some(season) => statement.query_map(
                params![media_id.to_string(), locale, season as i64],
                Self::map_episode_row,
            )?,
            None => statement.query_map(params![media_id.to_string(), locale], Self::map_episode_row)?,
        };
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn map_episode_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryEpisode> {
        let season: i64 = row.get(0)?;
        let episode: i64 = row.get(1)?;
        let runtime: Option<i64> = row.get(5)?;
        Ok(LibraryEpisode {
            season: season as u32,
            episode: episode as u32,
            title: row.get(2)?,
            overview: row.get(3)?,
            air_date: row.get(4)?,
            runtime_minutes: runtime.map(|minutes| minutes as u32),
            rating: row.get(6)?,
            still_url: row.get(7)?,
            owned: false,
            file_ids: Vec::new(),
        })
    }

    fn episode_files(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        season_filter: Option<u32>,
    ) -> Result<HashMap<(u32, u32), (Vec<LedgerId>, String, Option<String>)>, StoreError> {
        let sql = match season_filter {
            Some(_) => {
                "SELECT u.season, u.episode, f.file_id, f.display_path, f.still_path
                 FROM library_file_units u
                 JOIN library_files f ON f.file_id = u.file_id
                 WHERE u.library_id = ?1 AND u.media_id = ?2 AND u.season = ?3
                   AND u.season >= 0 AND u.episode >= 0
                   AND f.presence_state = 'present' AND f.publication_state = 'published'
                 ORDER BY u.season, u.episode, f.file_id"
            }
            None => {
                "SELECT u.season, u.episode, f.file_id, f.display_path, f.still_path
                 FROM library_file_units u
                 JOIN library_files f ON f.file_id = u.file_id
                 WHERE u.library_id = ?1 AND u.media_id = ?2
                   AND u.season >= 0 AND u.episode >= 0
                   AND f.presence_state = 'present' AND f.publication_state = 'published'
                 ORDER BY u.season, u.episode, f.file_id"
            }
        };
        let mapper = |row: &rusqlite::Row<'_>| {
            Ok((
                row.get::<_, i32>(0)?,
                row.get::<_, i32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        };
        let mut statement = self.library.prepare(sql)?;
        let rows = match season_filter {
            Some(season) => statement.query_map(
                params![library_id.to_string(), media_id.to_string(), season as i64],
                mapper,
            )?,
            None => statement.query_map(
                params![library_id.to_string(), media_id.to_string()],
                mapper,
            )?,
        };
        let mut out: HashMap<(u32, u32), (Vec<LedgerId>, String, Option<String>)> = HashMap::new();
        for row in rows {
            let (season, episode, file_id_str, path, still_path): (i32, i32, String, String, Option<String>) = row?;
            let key = (season as u32, episode as u32);
            let entry = out.entry(key).or_insert_with(|| (Vec::new(), path.clone(), still_path.clone()));
            if let Ok(id) = file_id_str.parse::<LedgerId>() {
                entry.0.push(id);
            }
        }
        Ok(out)
    }
}

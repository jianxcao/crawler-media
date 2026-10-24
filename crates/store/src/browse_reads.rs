//! Batch read facts for browse/playback consumers: media by id, and one
//! user's complete playback units.

use std::collections::{HashMap, HashSet};

use domain::{FileCoverage, Media, MediaId, UserId};
use rusqlite::params;

use super::maps::{LIBRARY_FILE_COLUMNS, group_file_coverage, map_file_unit, map_media, parse_id};
use super::media::MEDIA_SELECT;
use super::playback::UnitState;
use super::{LibraryOwnership, Store, StoreError};

/// One playback unit with its media id and full per-unit state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackUnitSnapshot {
    pub media_id: MediaId,
    pub season: i32,
    pub episode: i32,
    pub state: UnitState,
}

pub(super) const MEDIA_BATCH_CHUNK: usize = 500;

impl Store {
    /// Media rows for `ids`; unknown ids are simply absent, and empty input is
    /// an empty map. Ids are chunked to stay under SQLite's variable limit.
    pub fn media_batch(&self, ids: &[MediaId]) -> Result<HashMap<MediaId, Media>, StoreError> {
        let mut found = HashMap::new();
        for chunk in ids.chunks(MEDIA_BATCH_CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!("{MEDIA_SELECT} WHERE m.media_id IN ({placeholders})");
            let mut stmt = self.library.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().map(MediaId::to_string)),
                map_media,
            )?;
            for row in rows {
                let media = row?;
                found.insert(media.id, media);
            }
        }
        Ok(found)
    }

    /// Every playback unit for one user, with the same columns as
    /// `Store::unit_state` (position, play count, and track preferences
    /// included — not a reduced season/episode/played row).
    pub fn playback_units_for_user(
        &self,
        user: UserId,
    ) -> Result<Vec<PlaybackUnitSnapshot>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT media_id, season, episode,
                    position_ms, played, favorite, duration_ms, audio_track,
                    subtitle_track, play_count, watch_activity_at, favorite_at,
                    CASE WHEN has_played_assertion = 1 THEN COALESCE(played_changed_at_ms, 0) END
             FROM playback_units WHERE user_id = ?1
             ORDER BY media_id, season, episode",
        )?;
        let rows = stmt.query_map(params![user.to_string()], |row| {
            Ok(PlaybackUnitSnapshot {
                media_id: parse_id(row.get::<_, String>(0)?, 0)?,
                season: row.get(1)?,
                episode: row.get(2)?,
                state: UnitState {
                    position_ms: row.get(3)?,
                    played: row.get::<_, i64>(4)? != 0,
                    favorite: row.get::<_, i64>(5)? != 0,
                    duration_ms: row.get(6)?,
                    audio_track: row.get(7)?,
                    subtitle_track: row.get(8)?,
                    play_count: row.get(9)?,
                    updated_at: row.get(10)?,
                    favorite_at: row.get(11)?,
                    played_assertion_at_ms: row.get(12)?,
                },
            })
        })?;
        let mut units = Vec::new();
        for row in rows {
            units.push(row?);
        }
        Ok(units)
    }

    /// Playback units for a specific list of media ids, chunked to stay under SQLite variable limits.
    pub fn playback_units_for_media(
        &self,
        user: UserId,
        ids: &[MediaId],
    ) -> Result<Vec<PlaybackUnitSnapshot>, StoreError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut units = Vec::new();
        for chunk in ids.chunks(MEDIA_BATCH_CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT media_id, season, episode,
                        position_ms, played, favorite, duration_ms, audio_track,
                        subtitle_track, play_count, watch_activity_at, favorite_at,
                    CASE WHEN has_played_assertion = 1 THEN COALESCE(played_changed_at_ms, 0) END
                 FROM playback_units
                 WHERE user_id = ?1 AND media_id IN ({placeholders})
                 ORDER BY media_id, season, episode"
            );
            let mut stmt = self.library.prepare(&sql)?;
            let mut params_vec = Vec::with_capacity(1 + chunk.len());
            params_vec.push(rusqlite::types::Value::from(user.to_string()));
            for id in chunk {
                params_vec.push(rusqlite::types::Value::from(id.to_string()));
            }
            let rows = stmt.query_map(rusqlite::params_from_iter(params_vec), |row| {
                Ok(PlaybackUnitSnapshot {
                    media_id: parse_id(row.get::<_, String>(0)?, 0)?,
                    season: row.get(1)?,
                    episode: row.get(2)?,
                    state: UnitState {
                        position_ms: row.get(3)?,
                        played: row.get::<_, i64>(4)? != 0,
                        favorite: row.get::<_, i64>(5)? != 0,
                        duration_ms: row.get(6)?,
                        audio_track: row.get(7)?,
                        subtitle_track: row.get(8)?,
                        play_count: row.get(9)?,
                        updated_at: row.get(10)?,
                        favorite_at: row.get(11)?,
                        played_assertion_at_ms: row.get(12)?,
                    },
                })
            })?;
            for row in rows {
                units.push(row?);
            }
        }
        Ok(units)
    }

    /// Read files by their persisted Library ownership through the ownership index.
    pub fn ledger_candidates_for_library(
        &self,
        ownership: &LibraryOwnership,
        library_id: &str,
    ) -> Result<Vec<FileCoverage>, StoreError> {
        self.ledger_candidates_with_vm_steps(ownership, library_id)
            .map(|(rows, _)| rows)
    }

    /// Same read as [`Self::ledger_candidates_for_library`], additionally
    /// reporting the SQLite VM steps the candidate query consumed so tests can
    /// assert DB work instead of only returned rows.
    fn ledger_candidates_with_vm_steps(
        &self,
        ownership: &LibraryOwnership,
        library_id: &str,
    ) -> Result<(Vec<FileCoverage>, i64), StoreError> {
        if ownership.library(library_id).is_none() {
            return Ok((Vec::new(), 0));
        }
        let sql = format!(
            "SELECT {LIBRARY_FILE_COLUMNS}, u.season, u.episode
                   FROM library_files f
                   LEFT JOIN library_file_units u ON f.file_id = u.file_id
                   WHERE f.library_id = ?1"
        );
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map([library_id], map_file_unit)?;
        let collected = rows.collect::<Result<Vec<_>, _>>()?;
        let vm_steps = i64::from(stmt.get_status(rusqlite::StatementStatus::VmStep));
        drop(stmt);
        Ok((group_file_coverage(collected), vm_steps))
    }

    /// Restrict a Jellyfin parent request to ownership candidates before loading
    /// media and playback facts.
    pub fn jellyfin_ledger_candidates(
        &self,
        ownership: &LibraryOwnership,
        parent_id: Option<&str>,
    ) -> Result<Vec<FileCoverage>, StoreError> {
        match parent_id {
            Some(id) if ownership.library(id).is_some() => {
                self.ledger_candidates_for_library(ownership, id)
            }
            Some(id) => match domain::TypedItemId::protocol_media_id(id) {
                Some(media_id) => self.file_coverage_for_media(media_id),
                None => Ok(Vec::new()),
            },
            None => self.list_file_coverage(),
        }
    }

    pub fn jellyfin_ledger_candidates_with_vm_steps(
        &self,
        ownership: &LibraryOwnership,
        parent_id: &str,
    ) -> Result<(Vec<FileCoverage>, i64), StoreError> {
        self.ledger_candidates_with_vm_steps(ownership, parent_id)
    }

    /// Missing-file markers for the given ledger paths only, chunked under the
    /// SQLite variable limit. Unrelated missing rows are not read, so a growing
    /// instance ledger does not grow this read.
    pub fn missing_at_for_paths(&self, paths: &[String]) -> Result<HashSet<String>, StoreError> {
        let unique: HashSet<&str> = paths.iter().map(String::as_str).collect();
        let unique: Vec<&str> = unique.into_iter().collect();
        let mut missing = HashSet::new();
        for chunk in unique.chunks(MEDIA_BATCH_CHUNK) {
            // Keys are canonical at write time; canonicalize the probes once here
            // and hit the `path_key` UNIQUE index directly (no dual display read).
            let mut by_key: std::collections::HashMap<String, Vec<&str>> =
                std::collections::HashMap::new();
            for path in chunk {
                by_key
                    .entry(crate::path_key::normalize_path_key(path))
                    .or_default()
                    .push(path);
            }
            let placeholders = vec!["?"; by_key.len()].join(", ");
            let sql = format!(
                "SELECT path_key FROM library_files
                 WHERE missing_at IS NOT NULL AND path_key IN ({placeholders})"
            );
            let mut stmt = self.library.prepare(&sql)?;
            let keys: Vec<&String> = by_key.keys().collect();
            let rows = stmt.query_map(rusqlite::params_from_iter(keys), |row| {
                row.get::<_, String>(0)
            })?;
            for row in rows {
                for path in by_key.get(&row?).into_iter().flatten() {
                    missing.insert((*path).to_string());
                }
            }
        }
        Ok(missing)
    }
}

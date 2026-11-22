//! Read-only TV episode 5-state availability panorama across `library.db` and `subscribe.db`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use domain::{Confidence, LibraryId, MediaId, Release, UserId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Db, LibraryConnection, Store, StoreError};

const PANORAMA_READ_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeAvailabilityStatus {
    InLibrary,
    Downloading,
    FileMissing,
    Missing,
    Unaired,
}

impl EpisodeAvailabilityStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InLibrary => "in_library",
            Self::Downloading => "downloading",
            Self::FileMissing => "file_missing",
            Self::Missing => "missing",
            Self::Unaired => "unaired",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedEpisodeTarget {
    pub library_id: LibraryId,
    pub media_item_id: MediaId,
    pub resolution: Option<String>,
    pub file_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeAvailability {
    pub status: EpisodeAvailabilityStatus,
    pub owned_in: Option<OwnedEpisodeTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeasonAvailabilitySummary {
    pub season_number: u32,
    pub owned_count: usize,
    pub total_count: usize,
    pub downloading_count: usize,
}

#[derive(Clone, Debug, Default)]
pub struct EpisodePanorama {
    today_utc: String,
    owned_units: HashMap<(u32, u32), OwnedEpisodeTarget>,
    owned_rank: HashMap<(u32, u32), (i32, usize)>,
    missing_ledger_units: HashSet<(u32, u32)>,
    downloading_units: HashSet<(u32, u32)>,
    downloading_whole_seasons: HashSet<u32>,
    historical_fact_units: HashSet<(u32, u32)>,
    catalog_seasons: BTreeMap<u32, BTreeSet<u32>>,
}

impl EpisodePanorama {
    pub fn empty() -> Self {
        Self::new(current_utc_date())
    }

    fn new(today_utc: String) -> Self {
        Self {
            today_utc,
            ..Self::default()
        }
    }

    pub fn episode_availability(
        &self,
        season: u32,
        episode: u32,
        air_date: Option<&str>,
    ) -> EpisodeAvailability {
        let key = (season, episode);
        if let Some(owned) = self.owned_units.get(&key) {
            return EpisodeAvailability {
                status: EpisodeAvailabilityStatus::InLibrary,
                owned_in: Some(owned.clone()),
            };
        }
        if self.downloading_units.contains(&key) || self.downloading_whole_seasons.contains(&season)
        {
            return EpisodeAvailability {
                status: EpisodeAvailabilityStatus::Downloading,
                owned_in: None,
            };
        }
        if self.missing_ledger_units.contains(&key) || self.historical_fact_units.contains(&key) {
            return EpisodeAvailability {
                status: EpisodeAvailabilityStatus::FileMissing,
                owned_in: None,
            };
        }
        if is_future_air_date(air_date, &self.today_utc) {
            return EpisodeAvailability {
                status: EpisodeAvailabilityStatus::Unaired,
                owned_in: None,
            };
        }
        EpisodeAvailability {
            status: EpisodeAvailabilityStatus::Missing,
            owned_in: None,
        }
    }

    /// `(owned_episode_count, downloading_episode_count)` for a TMDB catalog season.
    pub fn counts_for_catalog_season(
        &self,
        season: u32,
        catalog_episode_count: u32,
    ) -> (usize, usize) {
        let owned_count = self
            .owned_units
            .keys()
            .filter(|(s, _)| *s == season)
            .count();
        let downloading_count = if self.downloading_whole_seasons.contains(&season) {
            let mut candidates: BTreeSet<u32> = (1..=catalog_episode_count).collect();
            if let Some(eps) = self.catalog_seasons.get(&season) {
                candidates.extend(eps.iter().copied());
            }
            for &(s, ep) in &self.downloading_units {
                if s == season {
                    candidates.insert(ep);
                }
            }
            candidates
                .into_iter()
                .filter(|ep| !self.owned_units.contains_key(&(season, *ep)))
                .count()
        } else {
            self.downloading_units
                .iter()
                .filter(|&&(s, ep)| s == season && !self.owned_units.contains_key(&(s, ep)))
                .count()
        };
        (owned_count, downloading_count)
    }

    pub fn record_cached_season_episodes(
        &mut self,
        season: u32,
        episodes: impl IntoIterator<Item = u32>,
    ) {
        self.catalog_seasons
            .entry(season)
            .or_default()
            .extend(episodes);
    }

    pub fn known_seasons(&self) -> BTreeSet<u32> {
        let mut seasons = BTreeSet::new();
        seasons.extend(self.owned_units.keys().map(|(s, _)| *s));
        seasons.extend(self.missing_ledger_units.iter().map(|(s, _)| *s));
        seasons.extend(self.catalog_seasons.keys().copied());
        seasons.extend(self.downloading_units.iter().map(|(s, _)| *s));
        seasons.extend(self.downloading_whole_seasons.iter().copied());
        seasons
    }

    pub fn concrete_units_in_season(&self, season: u32) -> BTreeSet<u32> {
        let mut units = BTreeSet::new();
        for &(s, ep) in self.owned_units.keys() {
            if s == season {
                units.insert(ep);
            }
        }
        for &(s, ep) in &self.missing_ledger_units {
            if s == season {
                units.insert(ep);
            }
        }
        for &(s, ep) in &self.downloading_units {
            if s == season {
                units.insert(ep);
            }
        }
        for &(s, ep) in &self.historical_fact_units {
            if s == season {
                units.insert(ep);
            }
        }
        units
    }

    pub fn library_season_summaries(&self) -> Vec<SeasonAvailabilitySummary> {
        self.known_seasons()
            .into_iter()
            .map(|season| {
                let mut known_eps = self
                    .catalog_seasons
                    .get(&season)
                    .cloned()
                    .unwrap_or_default();
                known_eps.extend(self.concrete_units_in_season(season));
                let owned_count = self
                    .owned_units
                    .keys()
                    .filter(|(s, _)| *s == season)
                    .count();
                let downloading_count = if self.downloading_whole_seasons.contains(&season) {
                    known_eps
                        .iter()
                        .filter(|ep| !self.owned_units.contains_key(&(season, **ep)))
                        .count()
                } else {
                    self.downloading_units
                        .iter()
                        .filter(|&&(s, ep)| s == season && !self.owned_units.contains_key(&(s, ep)))
                        .count()
                };
                SeasonAvailabilitySummary {
                    season_number: season,
                    owned_count,
                    total_count: known_eps.len(),
                    downloading_count,
                }
            })
            .collect()
    }

    fn load_library_facts(
        &mut self,
        db: &LibraryConnection<'_>,
        media_ids: &[MediaId],
        library_ids: &[LibraryId],
        preferred_library: Option<LibraryId>,
        locale: &str,
    ) -> Result<(), StoreError> {
        if media_ids.is_empty() || library_ids.is_empty() {
            return Ok(());
        }
        self.load_library_units(db.library, media_ids, library_ids)?;
        let mut seen = HashSet::new();
        for &media_id in media_ids {
            if !seen.insert(media_id) {
                continue;
            }
            let Some(resolved) = db.resolved_episode_locale(preferred_library, media_id, locale)?
            else {
                continue;
            };
            let mut stmt = db.library.prepare_cached(
                "SELECT season, episode
                 FROM media_episodes
                 WHERE media_id = ?1 AND locale = ?2 AND season >= 0 AND episode >= 0
                 ORDER BY season, episode",
            )?;
            let rows = stmt.query_map(params![media_id.to_string(), resolved], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (season, episode) = row?;
                self.catalog_seasons
                    .entry(season as u32)
                    .or_default()
                    .insert(episode as u32);
            }
        }
        Ok(())
    }

    fn load_library_units(
        &mut self,
        conn: &Connection,
        media_ids: &[MediaId],
        library_ids: &[LibraryId],
    ) -> Result<(), StoreError> {
        let lib_order: HashMap<LibraryId, usize> = library_ids
            .iter()
            .copied()
            .enumerate()
            .map(|(idx, id)| (id, idx))
            .collect();
        let media_json = serde_json::to_string(
            &media_ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        )?;
        let lib_json = serde_json::to_string(
            &library_ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        )?;
        let mut stmt = conn.prepare(
            "SELECT u.library_id, u.media_id, u.season, u.episode, u.file_count,
                    u.available_file_count, f.resolution
             FROM library_units u
             LEFT JOIN library_files f ON f.file_id = u.preferred_file_id
             WHERE u.media_id IN (SELECT value FROM json_each(?1))
               AND u.library_id IN (SELECT value FROM json_each(?2))
               AND u.season >= 0 AND u.episode >= 0
               AND NOT EXISTS (
                   SELECT 1 FROM library_removals r
                   WHERE r.library_id = u.library_id
               )",
        )?;
        let rows = stmt.query_map(params![media_json, lib_json], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        for row in rows {
            let (lib_raw, media_raw, season, episode, file_count, avail_count, resolution) = row?;
            let library_id: LibraryId = lib_raw.parse()?;
            let media_item_id: MediaId = media_raw.parse()?;
            let key = (season as u32, episode as u32);
            if avail_count > 0 {
                let rank = resolution_rank(resolution.as_deref());
                let order = lib_order.get(&library_id).copied().unwrap_or(usize::MAX);
                let replace = self.owned_rank.get(&key).is_none_or(|&(prev_r, prev_ord)| {
                    rank > prev_r || (rank == prev_r && order < prev_ord)
                });
                if replace {
                    self.owned_rank.insert(key, (rank, order));
                    self.owned_units.insert(
                        key,
                        OwnedEpisodeTarget {
                            library_id,
                            media_item_id,
                            resolution,
                            file_count: file_count as usize,
                        },
                    );
                }
            } else if file_count > 0 {
                self.missing_ledger_units.insert(key);
            }
        }
        Ok(())
    }

    fn load_subscribe_facts<F>(
        &mut self,
        conn: &Connection,
        media_ids: &[MediaId],
        library_ids: &[LibraryId],
        include_null_lib: bool,
        subscriber_filter: Option<Option<UserId>>,
        parse_release: &F,
    ) -> Result<(), StoreError>
    where
        F: Fn(&str) -> Release,
    {
        if media_ids.is_empty() || library_ids.is_empty() {
            return Ok(());
        }
        let filter_uid = match subscriber_filter {
            None => None,
            Some(Some(uid)) => Some(uid.to_string()),
            Some(None) => return Ok(()),
        };
        let media_json = serde_json::to_string(
            &media_ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        )?;
        let lib_json = serde_json::to_string(
            &library_ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        )?;
        let mut sub_stmt = conn.prepare(
            "SELECT id, coverage_kind, season, episode_from, episode_to
             FROM subscribes
             WHERE media_id IN (SELECT value FROM json_each(?1))
               AND (library_id IN (SELECT value FROM json_each(?2))
                    OR (library_id IS NULL AND ?3 != 0))
               AND (?4 IS NULL OR user_id = ?4)",
        )?;
        let mut sub_ids = Vec::new();
        let mut sub_cov: HashMap<String, Option<(u32, u32, Option<u32>)>> = HashMap::new();
        let sub_rows = sub_stmt.query_map(
            params![
                media_json,
                lib_json,
                i64::from(include_null_lib),
                filter_uid
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            },
        )?;
        for row in sub_rows {
            let (id, kind, season, ep_from, ep_to) = row?;
            let cov = match (kind.as_str(), season, ep_from) {
                ("tv", Some(s), Some(from)) if s >= 0 && from > 0 => Some((
                    s as u32,
                    from as u32,
                    ep_to.filter(|&v| v > 0).map(|v| v as u32),
                )),
                _ => None,
            };
            sub_cov.insert(id.clone(), cov);
            sub_ids.push(id);
        }
        if sub_ids.is_empty() {
            return Ok(());
        }
        let sub_json = serde_json::to_string(&sub_ids)?;
        self.load_wanted_and_facts(conn, &sub_json)?;
        self.load_pending_releases(conn, &sub_json, &sub_cov, parse_release)
    }

    fn load_wanted_and_facts(
        &mut self,
        conn: &Connection,
        sub_json: &str,
    ) -> Result<(), StoreError> {
        let mut wanted_stmt = conn.prepare(
            "SELECT season, episode
             FROM subscribe_wanted
             WHERE subscribe_id IN (SELECT value FROM json_each(?1))
               AND grabbed_at IS NOT NULL
               AND imported_at IS NULL",
        )?;
        let wanted_rows = wanted_stmt.query_map([sub_json], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in wanted_rows {
            let (season, episode) = row?;
            if season >= 0 && episode >= 0 {
                self.downloading_units
                    .insert((season as u32, episode as u32));
            } else if season >= 0 && episode == -1 {
                self.downloading_whole_seasons.insert(season as u32);
            }
        }

        let mut facts_stmt = conn.prepare(
            "SELECT season, episode
             FROM subscribe_facts
             WHERE subscribe_id IN (SELECT value FROM json_each(?1))
               AND path IS NOT NULL
               AND season >= 0 AND episode >= 0",
        )?;
        let fact_rows = facts_stmt.query_map([sub_json], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in fact_rows {
            let (season, episode) = row?;
            self.historical_fact_units
                .insert((season as u32, episode as u32));
        }
        Ok(())
    }

    fn load_pending_releases<F>(
        &mut self,
        conn: &Connection,
        sub_json: &str,
        sub_cov: &HashMap<String, Option<(u32, u32, Option<u32>)>>,
        parse_release: &F,
    ) -> Result<(), StoreError>
    where
        F: Fn(&str) -> Release,
    {
        let mut pending_stmt = conn.prepare(
            "SELECT subscribe_id, title, torrent_json
             FROM pending_downloads
             WHERE subscribe_id IN (SELECT value FROM json_each(?1))
               AND state IN ('active', 'stalled')",
        )?;
        let rows = pending_stmt.query_map([sub_json], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (sub_id, title, raw_json) = row?;
            let release = serde_json::from_str::<serde_json::Value>(&raw_json)
                .ok()
                .and_then(|val| release_from_override(val.get("release_override")))
                .unwrap_or_else(|| parse_release(&title));
            let cov = sub_cov.get(&sub_id).copied().flatten();
            self.record_pending_release(&release, cov);
        }
        Ok(())
    }

    fn record_pending_release(
        &mut self,
        release: &Release,
        coverage: Option<(u32, u32, Option<u32>)>,
    ) {
        let units = release.covered_episodes();
        if !units.is_empty() {
            for (parsed_s, ep) in units {
                let season = if release.season_inferred {
                    coverage.map(|(cs, _, _)| cs).unwrap_or(parsed_s)
                } else {
                    parsed_s
                };
                self.downloading_units.insert((season, ep));
            }
        } else if let Some(parsed_s) = release.season {
            let season = if release.season_inferred {
                coverage.map(|(cs, _, _)| cs).unwrap_or(parsed_s)
            } else {
                parsed_s
            };
            self.downloading_whole_seasons.insert(season);
        } else if let Some((cs, from, maybe_to)) = coverage {
            match maybe_to {
                None => {
                    self.downloading_whole_seasons.insert(cs);
                }
                Some(to) if to >= from && to - from < Release::MAX_PLAUSIBLE_FILE_EPISODES => {
                    for ep in from..=to {
                        self.downloading_units.insert((cs, ep));
                    }
                }
                _ => {}
            }
        }
    }
}

impl Store {
    /// Snapshot read across `library.db` -> `subscribe.db` without acquiring any writer lock.
    pub fn episode_panorama<F>(
        &self,
        media_ids: &[MediaId],
        library_ids: &[LibraryId],
        preferred_library: Option<LibraryId>,
        locale: &str,
        parse_release: F,
    ) -> Result<EpisodePanorama, StoreError>
    where
        F: Fn(&str) -> Release,
    {
        self.episode_panorama_scoped(
            media_ids,
            library_ids,
            preferred_library,
            locale,
            None,
            parse_release,
        )
    }

    /// Scoped snapshot read across `library.db` -> `subscribe.db` without acquiring any writer lock.
    pub fn episode_panorama_scoped<F>(
        &self,
        media_ids: &[MediaId],
        library_ids: &[LibraryId],
        preferred_library: Option<LibraryId>,
        locale: &str,
        subscriber_filter: Option<Option<UserId>>,
        parse_release: F,
    ) -> Result<EpisodePanorama, StoreError>
    where
        F: Fn(&str) -> Release,
    {
        let mut panorama = EpisodePanorama::new(current_utc_date());
        if media_ids.is_empty() || library_ids.is_empty() {
            return Ok(panorama);
        }
        let read_subscribe = !matches!(subscriber_filter, Some(None));
        let dbs: &[Db] = if read_subscribe {
            &[Db::Library, Db::Subscribe]
        } else {
            &[Db::Library]
        };
        self.with_read_snapshot(dbs, PANORAMA_READ_WAIT, || {
            let include_null_lib = self.with_read(Db::Library, |conn| {
                panorama.load_library_facts(
                    &self.library_connection(conn),
                    media_ids,
                    library_ids,
                    preferred_library,
                    locale,
                )?;
                default_tv_library_included(conn, library_ids)
            })?;
            if read_subscribe {
                self.with_read(Db::Subscribe, |conn| {
                    panorama.load_subscribe_facts(
                        conn,
                        media_ids,
                        library_ids,
                        include_null_lib,
                        subscriber_filter,
                        &parse_release,
                    )
                })?;
            }
            Ok(panorama)
        })?
    }
}

fn default_tv_library_included(
    conn: &Connection,
    library_ids: &[LibraryId],
) -> Result<bool, StoreError> {
    let default_raw: Option<String> = conn
        .query_row(
            "SELECT library_id FROM libraries
             WHERE kind = 'tv'
               AND is_default = 1
               AND lifecycle_state = 'ready'
               AND NOT EXISTS (
                   SELECT 1 FROM library_removals r
                   WHERE r.library_id = libraries.library_id
               )
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(default_raw
        .and_then(|raw| raw.parse::<LibraryId>().ok())
        .is_some_and(|default_id| library_ids.contains(&default_id)))
}

fn current_utc_date() -> String {
    let now = crate::now_rfc3339();
    now.split('T').next().unwrap_or(&now).to_string()
}

fn is_future_air_date(air_date: Option<&str>, today_utc: &str) -> bool {
    let Some(raw) = air_date.map(str::trim).filter(|s| s.len() >= 10) else {
        return false;
    };
    let b = raw.as_bytes();
    let valid = b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..10].iter().all(u8::is_ascii_digit);
    valid && &raw[..10] > today_utc
}

fn resolution_rank(resolution: Option<&str>) -> i32 {
    match resolution.map(str::trim) {
        Some(r) if r.eq_ignore_ascii_case("4320p") || r.eq_ignore_ascii_case("8k") => 5,
        Some(r) if r.eq_ignore_ascii_case("2160p") || r.eq_ignore_ascii_case("4k") => 4,
        Some(r) if r.eq_ignore_ascii_case("1440p") || r.eq_ignore_ascii_case("2k") => 3,
        Some(r) if r.eq_ignore_ascii_case("1080p") || r.eq_ignore_ascii_case("1080i") => 2,
        Some(r) if r.eq_ignore_ascii_case("720p") => 1,
        _ => 0,
    }
}

fn release_from_override(value: Option<&serde_json::Value>) -> Option<Release> {
    let value = value?;
    Some(Release {
        title: value["title"].as_str().unwrap_or_default().to_string(),
        year: value["year"].as_u64().map(|n| n as u16),
        season: value["season"].as_u64().map(|n| n as u32),
        episode: value["episode"].as_u64().map(|n| n as u32),
        episode_to: value["episode_to"].as_u64().map(|n| n as u32),
        resolution: value["resolution"].as_str().map(str::to_string),
        source: value["source"].as_str().map(str::to_string),
        codec: value["codec"].as_str().map(str::to_string),
        hdr: value["hdr"].as_str().map(str::to_string),
        subtitle_language: None,
        audio_language: None,
        group: None,
        confidence: Confidence::High,
        season_inferred: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_precedence_prefers_in_library_then_downloading_then_file_missing_then_unaired() {
        let mut p = EpisodePanorama::new("2026-06-15".into());
        let lib_id = LibraryId::new();
        let media_id = MediaId::new();
        p.owned_units.insert(
            (1, 1),
            OwnedEpisodeTarget {
                library_id: lib_id,
                media_item_id: media_id,
                resolution: Some("2160p".into()),
                file_count: 1,
            },
        );
        p.downloading_units.insert((1, 1));
        p.downloading_units.insert((1, 2));
        p.missing_ledger_units.insert((1, 2));
        p.missing_ledger_units.insert((1, 3));
        p.historical_fact_units.insert((1, 4));

        assert_eq!(
            p.episode_availability(1, 1, Some("2099-01-01")).status,
            EpisodeAvailabilityStatus::InLibrary
        );
        assert_eq!(
            p.episode_availability(1, 2, Some("2099-01-01")).status,
            EpisodeAvailabilityStatus::Downloading
        );
        assert_eq!(
            p.episode_availability(1, 3, Some("2099-01-01")).status,
            EpisodeAvailabilityStatus::FileMissing
        );
        assert_eq!(
            p.episode_availability(1, 4, Some("2020-01-01")).status,
            EpisodeAvailabilityStatus::FileMissing
        );
        assert_eq!(
            p.episode_availability(1, 5, Some("2026-06-16")).status,
            EpisodeAvailabilityStatus::Unaired
        );
        assert_eq!(
            p.episode_availability(1, 6, Some("2026-06-15")).status,
            EpisodeAvailabilityStatus::Missing
        );
        assert_eq!(
            p.episode_availability(1, 7, None).status,
            EpisodeAvailabilityStatus::Missing
        );
    }
}

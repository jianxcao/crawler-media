//! Transactional `user_library_items` writer.
//!
//! Browse readers (`library_query::items`, `library_query::facets`) treat a
//! missing row as "not favourite / not seen", so this module only materialises
//! a row only for user/Media pairs with raw facts, in every owning Library.
//! Every user-fact write calls [`Store::refresh_user_library_summary`] inside
//! its own transaction. The reduction reads the Library's owned units plus the
//! user's `playback_units` facts; it never derives a value from an existing
//! summary row, which is what keeps re-running a write a no-op.

use std::collections::{HashMap, HashSet};

use domain::{MediaId, UserId};
use rusqlite::{Connection, OptionalExtension, params};

use super::{Store, StoreError, UNIT_WHOLE};

mod continuation;
mod rows;
mod units;
const SUMMARY_COLUMNS: &str = "summary_revision,seen,played,favorite,last_played_at,favorite_at,owned_episode_count,completed_episode_count,next_season,next_episode,resume_season,resume_episode,activity_next_season,activity_next_episode,activity_season,activity_episode,activity_at,activity_present,all_watch_at,resume_activity_at";

/// One reduced row of `user_library_items`, compared field by field so an
/// unchanged write does not bump `summary_revision`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Summary {
    seen: bool,
    played: bool,
    favorite: bool,
    last_played_at: i64,
    favorite_at: i64,
    owned_episode_count: i64,
    completed_episode_count: i64,
    next_season: Option<i32>,
    next_episode: Option<i32>,
    resume_season: Option<i32>,
    resume_episode: Option<i32>,
    activity_next_season: Option<i32>,
    activity_next_episode: Option<i32>,
    activity_season: Option<i32>,
    activity_episode: Option<i32>,
    activity_at: i64,
    activity_present: bool,
    all_watch_at: i64,
    resume_activity_at: i64,
}

/// One `playback_units` row of the current user and media.
pub(crate) struct PlaybackFact {
    season: i32,
    episode: i32,
    played: bool,
    has_assertion: bool,
    played_at: i64,
    favorite: bool,
    favorite_at: i64,
    position_ms: i64,
    watch_activity_at: i64,
    play_count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserLibrarySummary {
    pub user_id: UserId,
    pub library_id: domain::LibraryId,
    pub media_id: MediaId,
    pub seen: bool,
    pub played: bool,
    pub favorite: bool,
    pub last_played_at: i64,
    pub favorite_at: i64,
    pub owned_episode_count: i64,
    pub completed_episode_count: i64,
    pub next_season: Option<i32>,
    pub next_episode: Option<i32>,
    pub resume_season: Option<i32>,
    pub resume_episode: Option<i32>,
    pub summary_revision: i64,
}

impl Store {
    pub fn get_user_library_summary(
        &self,
        user_id: UserId,
        library_id: domain::LibraryId,
        media_id: MediaId,
    ) -> Result<Option<UserLibrarySummary>, StoreError> {
        self.with_library_read(|db| db.get_user_library_summary(user_id, library_id, media_id))
    }

    /// Every Library owning the Media, in a deterministic
    /// order so concurrent writers take the same rows in the same sequence.
    fn summary_libraries(tx: &Connection, media_id: MediaId) -> Result<Vec<String>, StoreError> {
        query_ids(
            tx,
            "SELECT library_id FROM library_items WHERE media_id=?1 ORDER BY library_id",
            media_id,
        )
    }

    pub(crate) fn summary_facts(
        tx: &Connection,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<HashMap<(i32, i32), PlaybackFact>, StoreError> {
        let mut statement = tx.prepare(
            "SELECT season, episode, played, has_played_assertion, played_changed_at_ms,
                    favorite, favorite_at, position_ms, watch_activity_at, play_count
             FROM playback_units WHERE user_id = ?1 AND media_id = ?2",
        )?;
        let rows =
            statement.query_map(params![user_id.to_string(), media_id.to_string()], |row| {
                Ok(PlaybackFact {
                    season: row.get(0)?,
                    episode: row.get(1)?,
                    played: row.get::<_, i64>(2)? != 0,
                    has_assertion: row.get::<_, i64>(3)? != 0,
                    played_at: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    favorite: row.get::<_, i64>(5)? != 0,
                    favorite_at: row.get(6)?,
                    position_ms: row.get(7)?,
                    watch_activity_at: row.get(8)?,
                    play_count: row.get(9)?,
                })
            })?;
        let mut facts = HashMap::new();
        for row in rows {
            let fact = row?;
            facts.insert((fact.season, fact.episode), fact);
        }
        Ok(facts)
    }

    fn library_units(
        tx: &Connection,
        library_id: &str,
        media_id: MediaId,
    ) -> Result<Vec<(i32, i32)>, StoreError> {
        let mut statement = tx.prepare(
            "SELECT season, episode FROM library_units
             WHERE library_id = ?1 AND media_id = ?2 ORDER BY season, episode",
        )?;
        let rows = statement.query_map(params![library_id, media_id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn existing_summary(
        tx: &Connection,
        user_id: UserId,
        library_id: &str,
        media_id: MediaId,
    ) -> Result<Option<(i64, Summary)>, StoreError> {
        tx.query_row(
            &format!(
                "SELECT {SUMMARY_COLUMNS} FROM user_library_items
                 WHERE user_id = ?1 AND library_id = ?2 AND media_id = ?3"
            ),
            params![user_id.to_string(), library_id, media_id.to_string()],
            |row| Ok((row.get::<_, i64>(0)?, map_summary(row)?)),
        )
        .optional()
        .map_err(Into::into)
    }

    pub(crate) fn expected_user_rows(
        tx: &Connection,
        library: &str,
        media_id: MediaId,
        owned: &[(i32, i32)],
        available: &[(i32, i32)],
        is_tv: bool,
    ) -> Result<Vec<crate::query_projection::expected::OwnedRow>, StoreError> {
        let users = query_ids(
            tx,
            "SELECT DISTINCT user_id FROM playback_units WHERE media_id=?1 ORDER BY user_id",
            media_id,
        )?;
        let mut rows = Vec::new();
        for user in users {
            let facts = Self::summary_facts(tx, user.parse()?, media_id)?;
            if facts.is_empty() {
                continue;
            }
            let summary = summarize(is_tv, owned, available, &facts);
            rows.push(rows::owned(&summary, &user, library, media_id));
        }
        Ok(rows)
    }

    pub(crate) fn expected_continue_rows(
        tx: &Connection,
        library: &str,
        media_id: MediaId,
        available: &[(i32, i32)],
        is_tv: bool,
    ) -> Result<Vec<crate::query_projection::expected::OwnedRow>, StoreError> {
        let users = query_ids(
            tx,
            "SELECT DISTINCT user_id FROM playback_units WHERE media_id=?1 ORDER BY user_id",
            media_id,
        )?;
        let mut rows = Vec::new();
        for user in users {
            let facts = Self::summary_facts(tx, user.parse()?, media_id)?;
            if !facts.is_empty() {
                rows.extend(units::expected(
                    &user, library, media_id, is_tv, available, &facts,
                ));
            }
        }
        Ok(rows)
    }

    pub(crate) fn write_library_summary(
        tx: &Connection,
        user_id: UserId,
        media_id: MediaId,
        library_id: &str,
        is_tv: bool,
        facts: &HashMap<(i32, i32), PlaybackFact>,
    ) -> Result<(), StoreError> {
        let owned = Self::library_units(tx, library_id, media_id)?;
        let available = units::available(tx, library_id, media_id)?;
        let existing = Self::existing_summary(tx, user_id, library_id, media_id)?;
        if facts.is_empty() {
            tx.execute(
                "DELETE FROM user_library_items WHERE user_id=?1 AND library_id=?2 AND media_id=?3",
                params![user_id.to_string(), library_id, media_id.to_string()],
            )?;
            return Ok(());
        }
        let summary = summarize(is_tv, &owned, &available, facts);
        let revision = match existing {
            Some((revision, previous)) if previous == summary => revision,
            Some((revision, _)) => revision + 1,
            None => 1,
        };
        if existing.is_none_or(|(_, previous)| previous != summary) {
            rows::write(tx, &summary, user_id, library_id, media_id, revision)?;
        }
        units::sync(tx, user_id, library_id, media_id, is_tv, &available, facts)?;
        Ok(())
    }
}

/// One ordered text column of `media_id`-scoped rows.
fn query_ids(tx: &Connection, sql: &str, media_id: MediaId) -> Result<Vec<String>, StoreError> {
    let mut statement = tx.prepare(sql)?;
    let rows = statement.query_map(params![media_id.to_string()], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn map_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<Summary> {
    Ok(Summary {
        seen: row.get::<_, i64>(1)? != 0,
        played: row.get::<_, i64>(2)? != 0,
        favorite: row.get::<_, i64>(3)? != 0,
        last_played_at: row.get(4)?,
        favorite_at: row.get(5)?,
        owned_episode_count: row.get(6)?,
        completed_episode_count: row.get(7)?,
        next_season: row.get(8)?,
        next_episode: row.get(9)?,
        resume_season: row.get(10)?,
        resume_episode: row.get(11)?,
        activity_next_season: row.get(12)?,
        activity_next_episode: row.get(13)?,
        activity_season: row.get(14)?,
        activity_episode: row.get(15)?,
        activity_at: row.get(16)?,
        activity_present: row.get::<_, i64>(17)? != 0,
        all_watch_at: row.get(18)?,
        resume_activity_at: row.get(19)?,
    })
}

/// Effective `played` of one unit coordinate.
///
/// A `played` assertion inherits from the season mark and the whole mark; the
/// most recent assertion wins and an equal timestamp prefers the more specific
/// coordinate (episode, then season, then whole). Movies use only `(-1, -1)`.
pub(crate) fn effective_played(
    is_tv: bool,
    facts: &HashMap<(i32, i32), PlaybackFact>,
    season: i32,
    episode: i32,
) -> bool {
    let mut best: Option<(i64, u8, bool)> = None;
    let mut consider = |key: (i32, i32), rank: u8| {
        let Some(fact) = facts.get(&key) else {
            return;
        };
        if !fact.has_assertion {
            return;
        }
        let replace = match best {
            None => true,
            Some((best_at, best_rank, _)) => {
                fact.played_at > best_at || (fact.played_at == best_at && rank > best_rank)
            }
        };
        if replace {
            best = Some((fact.played_at, rank, fact.played));
        }
    };
    if (season, episode) == (UNIT_WHOLE, UNIT_WHOLE) {
        consider((UNIT_WHOLE, UNIT_WHOLE), if is_tv { 3 } else { 2 });
    } else if episode == UNIT_WHOLE {
        consider((season, UNIT_WHOLE), 2);
        consider((UNIT_WHOLE, UNIT_WHOLE), 1);
    } else {
        consider((season, episode), 3);
        consider((season, UNIT_WHOLE), 2);
        consider((UNIT_WHOLE, UNIT_WHOLE), 1);
    }
    best.map(|(_, _, played)| played).unwrap_or(false)
}

/// Reduce the Library's owned units and the user's facts into one summary row.
///
/// Browse display fields use only facts whose coordinate the Library owns
/// (whole marks apply to every owner). Continuation separately uses the user's
/// global Media activity/anchor plus this Library's available coordinates, so a
/// write to another season can advance continuation without changing local
/// seen/played/favorite/last-played display values.
fn summarize(
    is_tv: bool,
    owned: &[(i32, i32)],
    available: &[(i32, i32)],
    facts: &HashMap<(i32, i32), PlaybackFact>,
) -> Summary {
    let episodes: Vec<(i32, i32)> = owned
        .iter()
        .copied()
        .filter(|(season, episode)| *season >= 0 && *episode >= 0)
        .collect();
    let seasons: HashSet<i32> = episodes.iter().map(|(season, _)| *season).collect();
    // O03: O(1) membership instead of a linear scan per fact.
    let episode_set: HashSet<(i32, i32)> = episodes.iter().copied().collect();
    let applies = |fact: &PlaybackFact| -> bool {
        if !is_tv {
            return (fact.season, fact.episode) == (UNIT_WHOLE, UNIT_WHOLE);
        }
        match (fact.season, fact.episode) {
            (UNIT_WHOLE, UNIT_WHOLE) => true,
            (season, UNIT_WHOLE) => seasons.contains(&season),
            (season, episode) => episode_set.contains(&(season, episode)),
        }
    };

    let owned_episode_count = episodes.len() as i64;
    let completed_episode_count = episodes
        .iter()
        .filter(|(season, episode)| effective_played(is_tv, facts, *season, *episode))
        .count() as i64;
    let played = if is_tv && owned_episode_count > 0 {
        completed_episode_count == owned_episode_count
    } else {
        effective_played(is_tv, facts, UNIT_WHOLE, UNIT_WHOLE)
    };
    let seen = played
        || episodes
            .iter()
            .any(|(season, episode)| effective_played(is_tv, facts, *season, *episode))
        || facts
            .values()
            .any(|fact| applies(fact) && fact.position_ms > 0);

    let last_played_at = facts
        .values()
        .filter(|fact| applies(fact))
        .map(|fact| fact.watch_activity_at)
        .max()
        .unwrap_or(0);
    let favorite = facts.values().any(|fact| applies(fact) && fact.favorite);
    let favorite_at = facts
        .values()
        .filter(|fact| applies(fact) && fact.favorite)
        .map(|fact| fact.favorite_at)
        .max()
        .unwrap_or(0);

    let mut summary = Summary {
        seen,
        played,
        favorite,
        last_played_at,
        favorite_at,
        owned_episode_count,
        completed_episode_count,
        ..Default::default()
    };
    continuation::apply(&mut summary, is_tv, available, facts);
    summary
}

#[cfg(test)]
mod tests;

#[path = "connection.rs"]
mod connection;

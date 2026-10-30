//! Transactional `user_library_items` writer.
//!
//! Browse readers (`library_query::items`, `library_query::facets`) treat a
//! missing row as "not favourite / not seen", so this module only materialises
//! a row while the user actually carries state for the media in one Library.
//! Every user-fact write calls [`Store::refresh_user_library_summary`] inside
//! its own transaction. The reduction reads the Library's owned units plus the
//! user's `playback_units` facts; it never derives a value from an existing
//! summary row, which is what keeps re-running a write a no-op.

use std::collections::{HashMap, HashSet};

use domain::{MediaId, UserId};
use rusqlite::{Connection, OptionalExtension, params};

use super::{Store, StoreError, UNIT_WHOLE};

/// Which units a user-fact write touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SummaryScope {
    Episode { season: i32, episode: i32 },
    Season(i32),
    Whole,
}

impl SummaryScope {
    /// Scope of one written `playback_units` coordinate.
    pub(crate) fn for_unit(season: i32, episode: i32) -> Self {
        if (season, episode) == (UNIT_WHOLE, UNIT_WHOLE) {
            Self::Whole
        } else if episode == UNIT_WHOLE {
            Self::Season(season)
        } else {
            Self::Episode { season, episode }
        }
    }
}

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
}

impl Summary {
    /// Ownership and pointer fields alone are not user state: without a
    /// favourite or a viewing fact the row is deleted and readers fall back to
    /// their "missing = default" branch.
    fn is_empty(&self) -> bool {
        !self.seen
            && !self.played
            && !self.favorite
            && self.last_played_at == 0
            && self.favorite_at == 0
    }
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

    /// Libraries owning the media within the written scope, in a deterministic
    /// order so concurrent writers take the same rows in the same sequence.
    fn summary_libraries(
        tx: &Connection,
        media_id: MediaId,
        scope: SummaryScope,
        is_tv: bool,
    ) -> Result<Vec<String>, StoreError> {
        let media = media_id.to_string();
        // A movie/video has one logical whole unit.
        let (sql, values): (&str, Vec<rusqlite::types::Value>) = if !is_tv {
            (
                "SELECT library_id FROM library_items WHERE media_id = ?1 ORDER BY library_id",
                vec![media.into()],
            )
        } else {
            match scope {
                SummaryScope::Whole => (
                    "SELECT library_id FROM library_items WHERE media_id = ?1 ORDER BY library_id",
                    vec![media.into()],
                ),
                SummaryScope::Season(season) => (
                    "SELECT DISTINCT library_id FROM library_units
                     WHERE media_id = ?1 AND season = ?2 ORDER BY library_id",
                    vec![media.into(), i64::from(season).into()],
                ),
                SummaryScope::Episode { season, episode } => (
                    "SELECT DISTINCT library_id FROM library_units
                     WHERE media_id = ?1 AND season = ?2 AND episode = ?3 ORDER BY library_id",
                    vec![
                        media.into(),
                        i64::from(season).into(),
                        i64::from(episode).into(),
                    ],
                ),
            }
        };
        let mut statement = tx.prepare(sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(values), |row| {
            row.get::<_, String>(0)
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub(crate) fn summary_facts(
        tx: &Connection,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<HashMap<(i32, i32), PlaybackFact>, StoreError> {
        let mut statement = tx.prepare(
            "SELECT season, episode, played, has_played_assertion, played_changed_at_ms,
                    favorite, favorite_at, position_ms, watch_activity_at
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
            "SELECT summary_revision, seen, played, favorite, last_played_at, favorite_at,
                        owned_episode_count, completed_episode_count, next_season, next_episode,
                        resume_season, resume_episode
                 FROM user_library_items
                 WHERE user_id = ?1 AND library_id = ?2 AND media_id = ?3",
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
            let summary = summarize(is_tv, owned, &facts);
            if summary.is_empty() {
                continue;
            }
            use rusqlite::types::Value;
            let values = [
                ("user_id", Value::from(user)),
                ("library_id", library.to_string().into()),
                ("media_id", media_id.to_string().into()),
                ("seen", i64::from(summary.seen).into()),
                ("played", i64::from(summary.played).into()),
                ("favorite", i64::from(summary.favorite).into()),
                ("last_played_at", summary.last_played_at.into()),
                ("favorite_at", summary.favorite_at.into()),
                ("owned_episode_count", summary.owned_episode_count.into()),
                (
                    "completed_episode_count",
                    summary.completed_episode_count.into(),
                ),
                ("next_season", summary.next_season.into()),
                ("next_episode", summary.next_episode.into()),
                ("resume_season", summary.resume_season.into()),
                ("resume_episode", summary.resume_episode.into()),
            ];
            rows.push(
                values
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value))
                    .collect(),
            );
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
        let summary = summarize(is_tv, &owned, facts);
        let existing = Self::existing_summary(tx, user_id, library_id, media_id)?;
        let user = user_id.to_string();
        let media = media_id.to_string();

        if summary.is_empty() {
            if existing.is_some() {
                tx.execute(
                    "DELETE FROM user_library_items
                     WHERE user_id = ?1 AND library_id = ?2 AND media_id = ?3",
                    params![user, library_id, media],
                )?;
                tracing::debug!(
                    %user_id,
                    library_id,
                    %media_id,
                    "removed user library summary without state"
                );
            }
            return Ok(());
        }

        let revision = match existing {
            Some((_revision, previous)) if previous == summary => return Ok(()),
            Some((revision, _)) => revision + 1,
            None => 1,
        };
        tx.execute(
            "INSERT INTO user_library_items
                (user_id, library_id, media_id, seen, played, favorite, last_played_at,
                 favorite_at, owned_episode_count, completed_episode_count,
                 next_season, next_episode, resume_season, resume_episode, summary_revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(user_id, library_id, media_id) DO UPDATE SET
                seen = excluded.seen,
                played = excluded.played,
                favorite = excluded.favorite,
                last_played_at = excluded.last_played_at,
                favorite_at = excluded.favorite_at,
                owned_episode_count = excluded.owned_episode_count,
                completed_episode_count = excluded.completed_episode_count,
                next_season = excluded.next_season,
                next_episode = excluded.next_episode,
                resume_season = excluded.resume_season,
                resume_episode = excluded.resume_episode,
                summary_revision = excluded.summary_revision",
            params![
                user,
                library_id,
                media,
                i64::from(summary.seen),
                i64::from(summary.played),
                i64::from(summary.favorite),
                summary.last_played_at,
                summary.favorite_at,
                summary.owned_episode_count,
                summary.completed_episode_count,
                summary.next_season,
                summary.next_episode,
                summary.resume_season,
                summary.resume_episode,
                revision,
            ],
        )?;
        tracing::debug!(
            %user_id,
            library_id,
            %media_id,
            revision,
            "wrote user library summary"
        );
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
/// Only facts whose coordinate the Library owns contribute (a whole mark
/// applies to every Library owning the media). This is why a season mark never
/// changes a Library that only owns another season.
fn summarize(
    is_tv: bool,
    owned: &[(i32, i32)],
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

    // Resume points at the newest unit that is in progress and not effectively
    // played. A movie has no episode coordinate, so it points at the whole unit.
    let resume = facts
        .values()
        .filter(|fact| applies(fact) && fact.position_ms > 0)
        .filter(|fact| !effective_played(is_tv, facts, fact.season, fact.episode))
        .max_by_key(|fact| (fact.watch_activity_at, fact.season, fact.episode));
    let (resume_season, resume_episode) = match resume {
        Some(fact) if is_tv => (Some(fact.season), Some(fact.episode)),
        Some(_) => (Some(UNIT_WHOLE), Some(UNIT_WHOLE)),
        None => (None, None),
    };

    // Next-up only exists once something was completed; the pointer is the
    // first owned episode that is not effectively played.
    let next = if is_tv && completed_episode_count > 0 {
        episodes
            .iter()
            .copied()
            .filter(|(season, episode)| !effective_played(true, facts, *season, *episode))
            .min()
    } else {
        None
    };
    let (next_season, next_episode) = match next {
        Some((season, episode)) => (Some(season), Some(episode)),
        None => (None, None),
    };

    Summary {
        seen,
        played,
        favorite,
        last_played_at,
        favorite_at,
        owned_episode_count,
        completed_episode_count,
        next_season,
        next_episode,
        resume_season,
        resume_episode,
    }
}

#[cfg(test)]
mod tests;

#[path = "connection.rs"]
mod connection;

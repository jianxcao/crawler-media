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

impl Store {
    /// Recompute the affected `(user_id, library_id, media_id)` summaries.
    ///
    /// Runs inside the caller's transaction. Only libraries that own units in
    /// the written scope are visited; a whole-scope write fans out through
    /// `library_items` (the ownership fact), episode/season writes through
    /// `library_units`.
    pub(crate) fn refresh_user_library_summary(
        &self,
        user_id: UserId,
        media_id: MediaId,
        scope: SummaryScope,
    ) -> Result<(), StoreError> {
        let Some(media) = self.get_media(media_id)? else {
            return Ok(());
        };
        let is_tv = media.kind == domain::MediaKind::Tv;
        let libraries = Self::summary_libraries(&self.library, media_id, scope, is_tv)?;
        if libraries.is_empty() {
            return Ok(());
        }
        let facts = Self::summary_facts(&self.library, user_id, media_id)?;
        for library_id in libraries {
            Self::write_library_summary(&self.library, user_id, media_id, &library_id, is_tv, &facts)?;
        }
        Ok(())
    }

    /// Recompute the survivor's summaries after a Media merge moved the loser's
    /// playback facts onto it. Runs inside the caller's transaction, bounded to
    /// the users holding facts on the survivor and the libraries owning its
    /// files; the reduction never reads a summary row, so a re-run writes nothing.
    pub(crate) fn refresh_merged_summaries(
        &self,
        tx: &Connection,
        media_id: MediaId,
    ) -> Result<(), StoreError> {
        let Some(media) = self.get_media(media_id)? else {
            return Ok(());
        };
        let is_tv = media.kind == domain::MediaKind::Tv;
        let users = query_ids(
            tx,
            "SELECT DISTINCT user_id FROM playback_units WHERE media_id = ?1 ORDER BY user_id",
            media_id,
        )?;
        if users.is_empty() {
            return Ok(());
        }
        let libraries = query_ids(
            tx,
            "SELECT DISTINCT library_id FROM library_files WHERE media_id = ?1 ORDER BY library_id",
            media_id,
        )?;
        for user in &users {
            let user_id: UserId = user.parse()?;
            let facts = Self::summary_facts(tx, user_id, media_id)?;
            for library_id in &libraries {
                Self::write_library_summary(tx, user_id, media_id, library_id, is_tv, &facts)?;
            }
        }
        tracing::info!(%media_id, users = users.len(), libraries = libraries.len(), "refreshed merged media summaries");
        Ok(())
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
        // A movie/video has one logical unit: any write to one of its units is a
        // whole-media write, including a legacy `(0, 0)` favourite.
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
        let rows = statement.query_map(
            params![user_id.to_string(), media_id.to_string()],
            |row| {
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
            },
        )?;
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
        let rows = statement.query_map(
            params![library_id, media_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
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
            Some((revision, previous)) if previous == summary => return Ok(()),
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
/// coordinate (episode, then season, then whole). A movie's legacy `(0, 0)`
/// row is considered alongside its canonical `(-1, -1)` row.
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
    let applies = |fact: &PlaybackFact| -> bool {
        if !is_tv {
            return true;
        }
        match (fact.season, fact.episode) {
            (UNIT_WHOLE, UNIT_WHOLE) => true,
            (season, UNIT_WHOLE) => seasons.contains(&season),
            (season, episode) => episodes.contains(&(season, episode)),
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
    let favorite = facts
        .values()
        .any(|fact| applies(fact) && fact.favorite);
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
mod tests {
    use std::path::Path;

    use super::*;
    use crate::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
    use domain::{Confidence, LedgerId, LedgerRow, Media, MediaKind, QualitySource};

    fn open() -> (tempfile::TempDir, Store) {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("data")).unwrap();
        (temp, store)
    }

    fn tv_library(
        store: &Store,
        temp: &tempfile::TempDir,
        name: &str,
    ) -> (String, std::path::PathBuf) {
        let root = temp.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        let library = store
            .create_library(
                MediaKind::Tv,
                name,
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        (library.id, root)
    }

    fn seed_tv(store: &Store, title: &str) -> MediaId {
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: title.to_string(),
            year: Some(2024),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        media.id
    }

    fn seed_episode(store: &Store, root: &Path, media: MediaId, season: u32, episode: u32) {
        let row = LedgerRow {
            id: LedgerId::new(),
            media_id: media,
            path: root
                .join(format!("S{season:02}E{episode:02}.mkv"))
                .display()
                .to_string(),
            season: Some(season),
            episode: Some(episode),
            resolution: Some("1080p".into()),
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        };
        store.insert_ledger(&row).unwrap();
    }

    fn summary_row(
        store: &Store,
        user: UserId,
        library_id: &str,
        media: MediaId,
    ) -> Option<(i64, Summary)> {
        Store::existing_summary(&store.library, user, library_id, media).unwrap()
    }

    fn favorite_page(store: &Store, library_id: &str, user: UserId) -> usize {
        let query = LibraryQuery {
            library_id: library_id.parse().unwrap(),
            user_id: Some(user),
            filter: BrowseFilter {
                watch: Some("favorite".into()),
                ..BrowseFilter::default()
            },
            order: BrowseOrder::Title,
            descending: false,
            page: BrowsePage::Offset { start: 0, limit: 50 },
            include_total: true,
        };
        store.library_item_page(&query).unwrap().items.len()
    }

    #[test]
    fn favouriting_one_episode_marks_the_media_favourite_in_its_library() {
        let (temp, store) = open();
        let (library_id, root) = tv_library(&store, &temp, "tv");
        let media = seed_tv(&store, "Show");
        seed_episode(&store, &root, media, 1, 1);
        seed_episode(&store, &root, media, 1, 2);
        let user = UserId::new();

        store
            .set_unit_favorite(user, media, 1, 1, true, 1_000)
            .unwrap();

        let (_, row) = summary_row(&store, user, &library_id, media)
            .expect("a favourite creates a summary row");
        assert!(row.favorite);
        assert_eq!(row.favorite_at, 1_000);
        assert!(!row.seen, "a favourite alone is not a viewing fact");
        assert_eq!(favorite_page(&store, &library_id, user), 1);
    }

    #[test]
    fn season_mark_completes_the_owned_season_and_advances_next_up() {
        let (temp, store) = open();
        let (library_id, root) = tv_library(&store, &temp, "tv");
        let media = seed_tv(&store, "Show");
        seed_episode(&store, &root, media, 1, 1);
        seed_episode(&store, &root, media, 1, 2);
        seed_episode(&store, &root, media, 2, 1);
        let user = UserId::new();

        store
            .set_unit_marks(user, media, 1, UNIT_WHOLE, Some(true), None, 2_000)
            .unwrap();

        let (_, row) = summary_row(&store, user, &library_id, media)
            .expect("a mark creates a summary row");
        assert_eq!(row.owned_episode_count, 3);
        assert_eq!(
            row.completed_episode_count, 2,
            "both owned S01 episodes are complete"
        );
        assert!(row.seen);
        assert!(!row.played, "S02 is still unwatched");
        assert_eq!((row.next_season, row.next_episode), (Some(2), Some(1)));
        assert_eq!(row.last_played_at, 2_000);
    }

    #[test]
    fn season_mark_leaves_libraries_owning_other_seasons_untouched() {
        let (temp, store) = open();
        let (season_one, season_one_root) = tv_library(&store, &temp, "s1");
        let (season_two, season_two_root) = tv_library(&store, &temp, "s2");
        let media = seed_tv(&store, "Show");
        seed_episode(&store, &season_one_root, media, 1, 1);
        seed_episode(&store, &season_two_root, media, 2, 1);
        let user = UserId::new();

        // Give the S02 Library a row of its own, then plant a sentinel: a
        // season-1 write must not touch it at all.
        store
            .set_unit_favorite(user, media, 2, 1, true, 1_000)
            .unwrap();
        store
            .library
            .execute(
                "UPDATE user_library_items SET last_played_at = 7_777, summary_revision = 9
                 WHERE user_id = ?1 AND library_id = ?2 AND media_id = ?3",
                params![user.to_string(), &season_two, media.to_string()],
            )
            .unwrap();

        store
            .set_unit_marks(user, media, 1, UNIT_WHOLE, Some(true), None, 2_000)
            .unwrap();

        let (_, one) =
            summary_row(&store, user, &season_one, media).expect("S01 library updated");
        assert_eq!(one.completed_episode_count, 1);
        assert!(one.played);

        let (revision, two) =
            summary_row(&store, user, &season_two, media).expect("S02 row survives");
        assert_eq!(revision, 9);
        assert_eq!(two.last_played_at, 7_777);
    }

    #[test]
    fn replaying_the_same_write_is_a_no_op() {
        let (temp, store) = open();
        let (library_id, root) = tv_library(&store, &temp, "tv");
        let media = seed_tv(&store, "Show");
        seed_episode(&store, &root, media, 1, 1);
        let user = UserId::new();

        store
            .set_unit_marks(user, media, 1, 1, Some(true), Some(true), 3_000)
            .unwrap();
        let first = summary_row(&store, user, &library_id, media).unwrap();
        store
            .set_unit_marks(user, media, 1, 1, Some(true), Some(true), 3_000)
            .unwrap();
        assert_eq!(
            summary_row(&store, user, &library_id, media).unwrap(),
            first,
            "repeating a mark must not bump the revision"
        );

        store
            .upsert_unit(
                user,
                media,
                1,
                1,
                90_000,
                None,
                None,
                Some(600_000),
                None,
                None,
                false,
                4_000,
            )
            .unwrap();
        let with_progress = summary_row(&store, user, &library_id, media).unwrap();
        store
            .upsert_unit(
                user,
                media,
                1,
                1,
                90_000,
                None,
                None,
                Some(600_000),
                None,
                None,
                false,
                4_000,
            )
            .unwrap();
        assert_eq!(
            summary_row(&store, user, &library_id, media).unwrap(),
            with_progress,
            "repeating the same progress must be a no-op"
        );
        assert!(with_progress.1.seen);
    }

    #[test]
    fn a_library_that_does_not_own_the_media_gets_no_summary() {
        let (temp, store) = open();
        let (owning, root) = tv_library(&store, &temp, "owning");
        let (other, _) = tv_library(&store, &temp, "other");
        let media = seed_tv(&store, "Show");
        seed_episode(&store, &root, media, 1, 1);
        let user = UserId::new();

        store
            .set_unit_marks(
                user,
                media,
                UNIT_WHOLE,
                UNIT_WHOLE,
                Some(true),
                Some(true),
                5_000,
            )
            .unwrap();

        assert!(summary_row(&store, user, &owning, media).is_some());
        assert!(summary_row(&store, user, &other, media).is_none());
        let count: i64 = store
            .library
            .query_row(
                "SELECT COUNT(*) FROM user_library_items WHERE library_id = ?1",
                params![&other],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}

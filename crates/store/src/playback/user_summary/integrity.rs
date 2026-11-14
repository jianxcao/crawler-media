//! Constant-size user reduction state for the explicit Integrity owner.
use super::*;
use crate::query_projection::expected::{OwnedRow, integer};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct IntegrityUser {
    pub id: String,
    pub facts_after: Option<(i32, i32)>,
    pub units_after: Option<(i32, i32)>,
    pub facts_done: bool,
    pub has_facts: bool,
    is_tv: bool,
    whole: Option<PlaybackFact>,
    summary: Summary,
    anchor: Option<(i64, i32, i32)>,
    resume: Option<(i64, i32, i32)>,
}

impl IntegrityUser {
    pub(crate) fn new(id: String, is_tv: bool) -> Self {
        Self {
            id,
            facts_after: None,
            units_after: None,
            facts_done: false,
            has_facts: false,
            is_tv,
            whole: None,
            summary: Summary::default(),
            anchor: None,
            resume: None,
        }
    }

    pub(crate) fn facts_page(&mut self, conn: &Connection, media: &str) -> Result<(), StoreError> {
        let after = self.facts_after.unwrap_or((i32::MIN, i32::MIN));
        let sql = "SELECT season,episode,played,has_played_assertion,played_changed_at_ms,favorite,favorite_at,position_ms,watch_activity_at,play_count
            FROM playback_units WHERE user_id=?1 AND media_id=?2 AND (season,episode)>(?3,?4) ORDER BY season,episode LIMIT 100";
        let facts = conn
            .prepare_cached(sql)?
            .query_map(params![self.id, media, after.0, after.1], map_fact)?
            .collect::<Result<Vec<_>, _>>()?;
        for fact in &facts {
            self.has_facts = true;
            self.summary.all_watch_at = self.summary.all_watch_at.max(fact.watch_activity_at);
            if fact.position_ms > 0 || fact.played || fact.play_count > 0 {
                self.summary.activity_present = true;
                self.summary.activity_at = self.summary.activity_at.max(fact.watch_activity_at);
            }
            if fact.season >= 0 && fact.episode >= 0 {
                let anchor = (fact.watch_activity_at, fact.season, fact.episode);
                if self.anchor.is_none_or(|old| anchor > old) {
                    self.anchor = Some(anchor);
                }
            }
            if (fact.season, fact.episode) == (-1, -1) {
                self.whole = Some(fact.clone());
                self.local(fact);
            }
            self.facts_after = Some((fact.season, fact.episode));
        }
        self.facts_done = facts.len() < 100;
        Ok(())
    }

    pub(crate) fn units_page(
        &mut self,
        conn: &Connection,
        library: &str,
        media: MediaId,
        units: &[OwnedRow],
    ) -> Result<Vec<OwnedRow>, StoreError> {
        let mut out = Vec::new();
        for unit in units {
            let (season, episode) = (
                integer(unit, "season") as i32,
                integer(unit, "episode") as i32,
            );
            let mut facts = HashMap::new();
            if let Some(whole) = &self.whole {
                facts.insert((-1, -1), whole.clone());
            }
            for key in [(season, -1), (season, episode)] {
                if key == (-1, -1) {
                    continue;
                }
                if let Some(fact) = fact_at(conn, &self.id, media, key)? {
                    if self.is_tv {
                        self.local(&fact);
                    }
                    facts.insert(key, fact);
                }
            }
            let played = effective_played(self.is_tv, &facts, season, episode);
            if season >= 0 && episode >= 0 {
                self.summary.owned_episode_count += 1;
                self.summary.completed_episode_count += i64::from(played);
                self.summary.seen |= played;
            }
            if integer(unit, "available_file_count") > 0 {
                let state = facts.get(&(season, episode));
                if !played {
                    out.push(continue_row(
                        &self.id, library, media, season, episode, state,
                    ));
                    if self.is_tv && season >= 0 && episode >= 0 {
                        if self.summary.next_season.is_none() {
                            self.summary.next_season = Some(season);
                            self.summary.next_episode = Some(episode);
                        }
                        let anchor = self.anchor.map(|(_, s, e)| (s, e)).unwrap_or((0, 0));
                        if (season, episode) >= anchor
                            && self.summary.activity_next_season.is_none()
                        {
                            self.summary.activity_next_season = Some(season);
                            self.summary.activity_next_episode = Some(episode);
                        }
                    }
                    if let Some(fact) = state.filter(|f| f.position_ms > 0) {
                        let resume = (fact.watch_activity_at, season, episode);
                        if self.resume.is_none_or(|old| resume > old) {
                            self.resume = Some(resume);
                        }
                    }
                }
            }
            self.units_after = Some((season, episode));
        }
        Ok(out)
    }

    fn local(&mut self, fact: &PlaybackFact) {
        self.summary.seen |= fact.position_ms > 0;
        self.summary.last_played_at = self.summary.last_played_at.max(fact.watch_activity_at);
        self.summary.favorite |= fact.favorite;
        if fact.favorite {
            self.summary.favorite_at = self.summary.favorite_at.max(fact.favorite_at);
        }
    }

    pub(crate) fn finish(mut self, library: &str, media: MediaId) -> OwnedRow {
        let mut facts = HashMap::new();
        if let Some(whole) = self.whole {
            facts.insert((-1, -1), whole);
        }
        self.summary.played = if self.is_tv && self.summary.owned_episode_count > 0 {
            self.summary.owned_episode_count == self.summary.completed_episode_count
        } else {
            effective_played(self.is_tv, &facts, -1, -1)
        };
        self.summary.seen |= self.summary.played;
        self.summary.activity_season = self.anchor.map(|(_, s, _)| s);
        self.summary.activity_episode = self.anchor.map(|(_, _, e)| e);
        self.summary.resume_activity_at = self.resume.map(|(at, _, _)| at).unwrap_or(0);
        self.summary.resume_season = self.resume.map(|(_, s, _)| s);
        self.summary.resume_episode = self.resume.map(|(_, _, e)| e);
        rows::owned(&self.summary, &self.id, library, media)
    }
}

fn map_fact(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlaybackFact> {
    Ok(PlaybackFact {
        season: row.get(0)?,
        episode: row.get(1)?,
        played: row.get(2)?,
        has_assertion: row.get(3)?,
        played_at: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
        favorite: row.get(5)?,
        favorite_at: row.get(6)?,
        position_ms: row.get(7)?,
        watch_activity_at: row.get(8)?,
        play_count: row.get(9)?,
    })
}

fn fact_at(
    conn: &Connection,
    user: &str,
    media: MediaId,
    key: (i32, i32),
) -> Result<Option<PlaybackFact>, StoreError> {
    Ok(conn.query_row("SELECT season,episode,played,has_played_assertion,played_changed_at_ms,favorite,favorite_at,position_ms,watch_activity_at,play_count
        FROM playback_units WHERE user_id=?1 AND media_id=?2 AND season=?3 AND episode=?4",params![user,media.to_string(),key.0,key.1],map_fact).optional()?)
}

fn continue_row(
    user: &str,
    library: &str,
    media: MediaId,
    season: i32,
    episode: i32,
    fact: Option<&PlaybackFact>,
) -> OwnedRow {
    OwnedRow::from([
        ("user_id".into(), user.to_string().into()),
        ("library_id".into(), library.to_string().into()),
        ("media_id".into(), media.to_string().into()),
        ("season".into(), season.into()),
        ("episode".into(), episode.into()),
        (
            "position_ms".into(),
            fact.map_or(0, |f| f.position_ms).into(),
        ),
        (
            "watch_activity_at".into(),
            fact.map_or(0, |f| f.watch_activity_at).into(),
        ),
    ])
}

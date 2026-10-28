//! Explicit `played` assertions and the effective unit state derived from them.
//!
//! A unit's "watched" fact is written only by an explicit action, so it is stored as an
//! assertion: `has_played_assertion` says the user *decided* something, and
//! `played_changed_at_ms` says when. Playback progress never rewrites it, which is why a
//! later heartbeat cannot un-finish an episode the user marked by hand.
//!
//! Marking a season (or the whole item) writes one assertion on that scope; readers must
//! therefore resolve the *effective* state instead of looking at one row. The precedence is
//! the newest assertion, and on an equal change time the more specific scope: episode >
//! season > whole item. Favourites are independent of all of this and never move with a
//! played assertion or a history clear.

use std::collections::{HashMap, HashSet};

use domain::{MediaId, UnitKey, UserId};
use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError, UNIT_WHOLE, UnitState};

/// How specific the assertion that decides a unit's `played` fact is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlayedAssertionScope {
    /// `(-1, -1)`: the item as a whole.
    WholeItem,
    /// `(season, -1)`: the season containing the unit.
    Season,
    /// `(season, episode)`: the unit itself.
    Episode,
}

impl PlayedAssertionScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WholeItem => "whole",
            Self::Season => "season",
            Self::Episode => "episode",
        }
    }
}

/// The assertion that decides what a unit reads as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectiveAssertion {
    pub scope: PlayedAssertionScope,
    pub played: bool,
}

/// One user's `played` assertions on one Media, loaded in a single read.
///
/// A season or item mark stores one row on that scope, so a reader resolving many
/// units of the same Media (an episode list, an up-next decision, a per-season
/// count) would otherwise repeat the three-layer lookup per unit. The snapshot
/// answers the same precedence rule from memory. It is a point-in-time copy:
/// resolve every unit of a request from the same instance, then drop it.
#[derive(Default)]
pub struct PlayedAssertionSnapshot {
    rows: HashMap<(i32, i32), (bool, i64)>,
}

impl PlayedAssertionSnapshot {
    /// Which assertion (and value) decides this unit's `played` fact.
    pub fn effective_played_assertion(&self, unit: UnitKey) -> Option<EffectiveAssertion> {
        pick_assertion(unit, |season, episode| {
            self.rows.get(&(season, episode)).copied()
        })
    }

    /// `unit`'s state with `played` resolved, taking the caller's own row (progress,
    /// favourite) as the base. `None` means this user carries no fact at all.
    pub fn effective_unit_state(&self, unit: UnitKey, own: Option<UnitState>) -> Option<UnitState> {
        resolve_played(own, self.effective_played_assertion(unit))
    }
}

impl Store {
    /// A unit's state with `played` resolved through the assertion hierarchy.
    ///
    /// `None` means the user carries no fact at all (no own row and no assertion) for this unit (not "unplayed"): a reader
    /// that needs a default should use that default, not an invented row.
    pub fn effective_unit_state(
        &self,
        user: UserId,
        media: MediaId,
        unit: UnitKey,
    ) -> Result<Option<UnitState>, StoreError> {
        let own = self.unit_state(user, media, unit.season, unit.episode)?;
        let assertion = self.effective_played_assertion(user, media, unit)?;
        Ok(resolve_played(own, assertion))
    }

    /// Which assertion (and value) decides this unit's `played` fact.
    pub fn effective_played_assertion(
        &self,
        user: UserId,
        media: MediaId,
        unit: UnitKey,
    ) -> Result<Option<EffectiveAssertion>, StoreError> {
        let mut lookup = |season: i32, episode: i32| -> Result<Option<(bool, i64)>, StoreError> {
            let row: Option<(bool, i64, Option<i64>)> = self
                .library
                .query_row(
                    "SELECT played, has_played_assertion, played_changed_at_ms FROM playback_units
                     WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
                    params![user.to_string(), media.to_string(), season, episode],
                    |r| Ok((r.get::<_, i64>(0)? != 0, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            Ok(row
                .filter(|(_, asserted, _)| *asserted == 1)
                .map(|(played, _, changed_at)| (played, changed_at.unwrap_or(0))))
        };
        lookup_assertion(unit, &mut lookup)
    }

    /// Every `played` assertion this user holds on `media`, as one read.
    ///
    /// Use this when a request resolves more than one unit of the same Media; a
    /// single unit is cheaper through [`Store::effective_unit_state`].
    pub fn played_assertion_snapshot(
        &self,
        user: UserId,
        media: MediaId,
    ) -> Result<PlayedAssertionSnapshot, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT season, episode, played, played_changed_at_ms FROM playback_units
             WHERE user_id = ?1 AND media_id = ?2 AND has_played_assertion = 1",
        )?;
        let rows = stmt.query_map(params![user.to_string(), media.to_string()], |row| {
            Ok((
                (row.get::<_, i32>(0)?, row.get::<_, i32>(1)?),
                (
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                ),
            ))
        })?;
        Ok(PlayedAssertionSnapshot {
            rows: rows.collect::<Result<_, _>>()?,
        })
    }

    /// Effective states for many units of one Media, in declaration order.
    ///
    /// Only requested units and their assertion ancestors are read. Large requests
    /// use bounded key windows in one SQLite snapshot.
    pub fn effective_unit_states(
        &self,
        user: UserId,
        media: MediaId,
        units: &[UnitKey],
    ) -> Result<Vec<Option<UnitState>>, StoreError> {
        if units.is_empty() {
            return Ok(Vec::new());
        }
        let mut keys = HashSet::new();
        for unit in units {
            keys.extend(
                candidates(*unit)
                    .into_iter()
                    .map(|(_, season, episode)| (season, episode)),
            );
            keys.insert((unit.season, unit.episode));
        }
        let mut keys: Vec<_> = keys.into_iter().collect();
        keys.sort_unstable();
        let transaction = if self.library.is_autocommit() {
            Some(self.library.unchecked_transaction()?)
        } else {
            None
        };
        let mut own = HashMap::new();
        for chunk in keys.chunks(400) {
            own.extend(read_unit_window(&self.library, user, media, chunk)?);
        }
        if let Some(transaction) = transaction {
            transaction.commit()?;
        }
        let snapshot = PlayedAssertionSnapshot {
            rows: own
                .iter()
                .filter_map(|(key, state)| {
                    state
                        .played_assertion_at_ms
                        .map(|at| (*key, (state.played, at)))
                })
                .collect(),
        };
        Ok(units
            .iter()
            .map(|unit| {
                snapshot.effective_unit_state(*unit, own.get(&(unit.season, unit.episode)).cloned())
            })
            .collect())
    }
}

fn read_unit_window(
    connection: &rusqlite::Connection,
    user: UserId,
    media: MediaId,
    keys: &[(i32, i32)],
) -> Result<HashMap<(i32, i32), UnitState>, StoreError> {
    use rusqlite::types::Value;
    let values = vec!["(?,?)"; keys.len()].join(",");
    let sql = format!("WITH requested(season, episode) AS (VALUES {values})
            SELECT p.season, p.episode, p.position_ms, p.played, p.favorite, p.duration_ms,
                   p.audio_track, p.subtitle_track, p.play_count, p.watch_activity_at, p.favorite_at,
                   CASE WHEN p.has_played_assertion = 1 THEN COALESCE(p.played_changed_at_ms, 0) END
            FROM requested r JOIN playback_units p ON p.user_id = ? AND p.media_id = ?
                 AND p.season = r.season AND p.episode = r.episode");
    let mut bindings: Vec<Value> = keys
        .iter()
        .flat_map(|(season, episode)| {
            [
                Value::Integer(*season as i64),
                Value::Integer(*episode as i64),
            ]
        })
        .collect();
    bindings.push(Value::Text(user.to_string()));
    bindings.push(Value::Text(media.to_string()));
    let mut stmt = connection.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(bindings), |row| {
        Ok((
            (row.get::<_, i32>(0)?, row.get::<_, i32>(1)?),
            UnitState {
                position_ms: row.get(2)?,
                played: row.get::<_, i64>(3)? != 0,
                favorite: row.get::<_, i64>(4)? != 0,
                duration_ms: row.get(5)?,
                audio_track: row.get(6)?,
                subtitle_track: row.get(7)?,
                play_count: row.get(8)?,
                updated_at: row.get(9)?,
                favorite_at: row.get(10)?,
                played_assertion_at_ms: row.get(11)?,
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The assertion scopes that can decide `unit`, least specific first.
fn candidates(unit: UnitKey) -> Vec<(PlayedAssertionScope, i32, i32)> {
    let mut scopes = vec![
        (PlayedAssertionScope::WholeItem, UNIT_WHOLE, UNIT_WHOLE),
        (PlayedAssertionScope::Season, unit.season, UNIT_WHOLE),
    ];
    // A whole-item unit has no episode row of its own: `(unit.season, unit.episode)`
    // would be `(-1, -1)`, the item scope again.
    if !unit.is_whole() {
        scopes.push((PlayedAssertionScope::Episode, unit.season, unit.episode));
    }
    scopes
}

/// Newest assertion wins; on an equal change time the more specific scope wins.
///
/// `played_clause` in the protocol SQL orders identically, so a Jellyfin view and
/// this reader never disagree about a unit.
fn pick_assertion(
    unit: UnitKey,
    lookup: impl Fn(i32, i32) -> Option<(bool, i64)>,
) -> Option<EffectiveAssertion> {
    let mut best: Option<((i64, u8), EffectiveAssertion)> = None;
    for (specificity, (scope, season, episode)) in candidates(unit).into_iter().enumerate() {
        let Some((played, changed_at)) = lookup(season, episode) else {
            continue;
        };
        let key = (changed_at, specificity as u8);
        if best.as_ref().is_none_or(|(current, _)| key > *current) {
            best = Some((key, EffectiveAssertion { scope, played }));
        }
    }
    best.map(|(_, assertion)| assertion)
}

fn lookup_assertion(
    unit: UnitKey,
    lookup: &mut impl FnMut(i32, i32) -> Result<Option<(bool, i64)>, StoreError>,
) -> Result<Option<EffectiveAssertion>, StoreError> {
    let mut best: Option<((i64, u8), EffectiveAssertion)> = None;
    for (specificity, (scope, season, episode)) in candidates(unit).into_iter().enumerate() {
        let Some((played, changed_at)) = lookup(season, episode)? else {
            continue;
        };
        let key = (changed_at, specificity as u8);
        if best.as_ref().is_none_or(|(current, _)| key > *current) {
            best = Some((key, EffectiveAssertion { scope, played }));
        }
    }
    Ok(best.map(|(_, assertion)| assertion))
}

/// Apply a resolved assertion to the caller's own row.
fn resolve_played(
    own: Option<UnitState>,
    assertion: Option<EffectiveAssertion>,
) -> Option<UnitState> {
    let Some(assertion) = assertion else {
        // A favourite-only or progress-only row is still a fact of this user.
        return own;
    };
    match own {
        Some(mut state) => {
            // The row exists for progress/favourite; `played` comes from the assertion,
            // never from the row's own flag alone.
            state.played = assertion.played;
            Some(state)
        }
        None => {
            // A season/item mark with no per-episode row: the unit reads as marked,
            // carrying no progress of its own.
            Some(UnitState {
                played: assertion.played,
                ..UnitState::default()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::PlaybackMutation;

    fn mark(
        store: &Store,
        user: UserId,
        media: MediaId,
        season: i32,
        episode: i32,
        played: bool,
        at: i64,
    ) {
        let change = PlaybackMutation::marks(user, season, episode, Some(played), None);
        store.mutate_playback(media, &change, at).unwrap();
    }

    fn fixture() -> (tempfile::TempDir, Store, UserId, MediaId) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let user = UserId::new();
        let media = MediaId::new();
        store
            .insert_media(&domain::Media {
                id: media,
                kind: domain::MediaKind::Tv,
                title: "Snapshot".into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        (dir, store, user, media)
    }

    #[test]
    fn snapshot_resolves_the_same_units_as_single_lookups() {
        let (_dir, store, user, media) = fixture();
        mark(&store, user, media, UNIT_WHOLE, UNIT_WHOLE, true, 10);
        mark(&store, user, media, 1, UNIT_WHOLE, false, 20);
        mark(&store, user, media, 1, 1, true, 20);
        mark(&store, user, media, 2, 1, false, 5);
        let snapshot = store.played_assertion_snapshot(user, media).unwrap();
        let units = [
            UnitKey::WHOLE,
            UnitKey::new(1, UNIT_WHOLE),
            UnitKey::new(1, 1),
            UnitKey::new(1, 2),
            UnitKey::new(2, 1),
            UnitKey::new(3, 1),
        ];
        for unit in units {
            assert_eq!(
                snapshot.effective_played_assertion(unit),
                store.effective_played_assertion(user, media, unit).unwrap(),
                "{unit:?}"
            );
        }
        assert_eq!(
            snapshot.effective_played_assertion(UnitKey::new(1, 1)),
            Some(EffectiveAssertion {
                scope: PlayedAssertionScope::Episode,
                played: true,
            })
        );
        // Season 2's newest assertion is the whole-item mark at 10.
        assert_eq!(
            snapshot.effective_played_assertion(UnitKey::new(2, 1)),
            Some(EffectiveAssertion {
                scope: PlayedAssertionScope::WholeItem,
                played: true,
            })
        );
    }

    #[test]
    fn snapshot_keeps_its_own_point_in_time_copy() {
        let (_dir, store, user, media) = fixture();
        mark(&store, user, media, UNIT_WHOLE, UNIT_WHOLE, true, 10);
        let snapshot = store.played_assertion_snapshot(user, media).unwrap();
        mark(&store, user, media, UNIT_WHOLE, UNIT_WHOLE, false, 30);
        assert!(
            snapshot
                .effective_played_assertion(UnitKey::new(2, 1))
                .unwrap()
                .played
        );
        assert!(
            !store
                .effective_unit_state(user, media, UnitKey::new(2, 1))
                .unwrap()
                .unwrap()
                .played
        );
    }

    #[test]
    fn other_users_and_untouched_units_carry_no_fact() {
        let (_dir, store, user, media) = fixture();
        mark(&store, user, media, 1, 1, true, 10);
        let other = UserId::new();
        assert!(
            store
                .played_assertion_snapshot(other, media)
                .unwrap()
                .effective_played_assertion(UnitKey::new(1, 1))
                .is_none()
        );
        let snapshot = store.played_assertion_snapshot(user, media).unwrap();
        assert!(
            snapshot
                .effective_played_assertion(UnitKey::new(1, 2))
                .is_none()
        );
        assert!(
            snapshot
                .effective_unit_state(UnitKey::new(1, 2), None)
                .is_none()
        );
    }

    #[test]
    fn batch_states_match_single_unit_reads_in_order() {
        let (_dir, store, user, media) = fixture();
        mark(&store, user, media, UNIT_WHOLE, UNIT_WHOLE, true, 10);
        mark(&store, user, media, 1, 2, false, 20);
        let units = [UnitKey::new(1, 1), UnitKey::new(1, 2), UnitKey::new(1, 3)];
        let batch = store.effective_unit_states(user, media, &units).unwrap();
        assert_eq!(batch.len(), units.len());
        for (index, unit) in units.into_iter().enumerate() {
            assert_eq!(
                batch[index],
                store.effective_unit_state(user, media, unit).unwrap(),
                "{unit:?}"
            );
        }
        assert_eq!(batch[0].as_ref().unwrap().played, true);
        assert_eq!(batch[1].as_ref().unwrap().played, false);
    }
}

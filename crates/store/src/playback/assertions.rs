//! Explicit `played` assertions and the effective unit state derived from them.
//!
//! A unit's "watched" fact is written only by an explicit action, so it is stored as an
//! assertion: `has_played_assertion` says the user *decided* something, and
//! `played_changed_at_ms` says when. Playback progress never rewrites it, which is why a
//! later heartbeat cannot un-finish an episode the user marked by hand.
//!
//! Marking a season (or the whole item) writes one assertion on that scope; readers must
//! therefore resolve the *effective* state instead of looking at one row. The precedence is
//! the most specific assertion: episode > season > whole item. Favourites are independent of
//! all of this and never move with a played assertion or a history clear.

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

impl Store {
    /// A unit's state with `played` resolved through the assertion hierarchy.
    ///
    /// `None` means the user carries no fact at all for this unit (not "unplayed"): a reader
    /// that needs a default should use that default, not an invented row.
    pub fn effective_unit_state(
        &self,
        user: UserId,
        media: MediaId,
        unit: UnitKey,
    ) -> Result<Option<UnitState>, StoreError> {
        let Some(assertion) = self.effective_played_assertion(user, media, unit)? else {
            return Ok(None);
        };
        let own = self.unit_state(user, media, unit.season, unit.episode)?;
        match own {
            Some(mut state) => {
                // The row exists for progress/favourite; `played` comes from the assertion,
                // never from the row's own flag alone.
                state.played = assertion.played;
                Ok(Some(state))
            }
            None => {
                // A season/item mark with no per-episode row: the unit reads as marked,
                // carrying no progress of its own.
                Ok(Some(UnitState {
                    played: assertion.played,
                    ..UnitState::default()
                }))
            }
        }
    }

    /// Which assertion (and value) decides this unit's `played` fact.
    pub fn effective_played_assertion(
        &self,
        user: UserId,
        media: MediaId,
        unit: UnitKey,
    ) -> Result<Option<EffectiveAssertion>, StoreError> {
        let candidates = [
            (PlayedAssertionScope::Episode, unit.season, unit.episode),
            (PlayedAssertionScope::Season, unit.season, UNIT_WHOLE),
            (PlayedAssertionScope::WholeItem, UNIT_WHOLE, UNIT_WHOLE),
        ];
        for (scope, season, episode) in candidates {
            if scope == PlayedAssertionScope::Episode && unit.is_whole() {
                continue;
            }
            let row: Option<(bool, i64)> = self
                .library
                .query_row(
                    "SELECT played, has_played_assertion FROM playback_units
                     WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
                    params![user.to_string(), media.to_string(), season, episode],
                    |r| Ok((r.get::<_, i64>(0)? != 0, r.get(1)?)),
                )
                .optional()?;
            let Some((played, has_assertion)) = row else {
                continue;
            };
            if has_assertion == 0 {
                continue;
            }
            return Ok(Some(EffectiveAssertion { scope, played }));
        }
        Ok(None)
    }
}

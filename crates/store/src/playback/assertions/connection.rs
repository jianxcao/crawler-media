//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
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
            Some(crate::read_tx(self.library)?)
        } else {
            None
        };
        let mut own = HashMap::new();
        for chunk in keys.chunks(400) {
            own.extend(read_unit_window(self.library, user, media, chunk)?);
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

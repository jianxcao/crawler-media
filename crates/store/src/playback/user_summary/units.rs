//! Sparse available effective-unplayed coordinates; unchanged heartbeats write no rows.
use super::*;
use crate::query_projection::expected::OwnedRow;
use std::collections::BTreeMap;

type Coordinates = BTreeMap<(i32, i32), (i64, i64)>;
pub(super) fn available(
    tx: &Connection,
    library: &str,
    media: MediaId,
) -> Result<Vec<(i32, i32)>, StoreError> {
    Ok(tx
        .prepare_cached(
            "SELECT season,episode FROM library_units WHERE library_id=?1 AND media_id=?2
        AND available_file_count>0 ORDER BY season,episode",
        )?
        .query_map(params![library, media.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<Result<_, _>>()?)
}
fn derive(
    is_tv: bool,
    available: &[(i32, i32)],
    facts: &HashMap<(i32, i32), PlaybackFact>,
) -> Coordinates {
    available
        .iter()
        .filter(|&&(season, episode)| !effective_played(is_tv, facts, season, episode))
        .map(|&unit| {
            let state = facts
                .get(&unit)
                .map(|fact| (fact.position_ms, fact.watch_activity_at))
                .unwrap_or_default();
            (unit, state)
        })
        .collect()
}
pub(super) fn expected(
    user: &str,
    library: &str,
    media: MediaId,
    is_tv: bool,
    available: &[(i32, i32)],
    facts: &HashMap<(i32, i32), PlaybackFact>,
) -> Vec<OwnedRow> {
    derive(is_tv, available, facts)
        .into_iter()
        .map(|((season, episode), (position, clock))| {
            [
                ("user_id", rusqlite::types::Value::from(user.to_string())),
                ("library_id", library.to_string().into()),
                ("media_id", media.to_string().into()),
                ("season", season.into()),
                ("episode", episode.into()),
                ("position_ms", position.into()),
                ("watch_activity_at", clock.into()),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
        })
        .collect()
}
pub(super) fn sync(
    tx: &Connection,
    user: UserId,
    library: &str,
    media: MediaId,
    is_tv: bool,
    available: &[(i32, i32)],
    facts: &HashMap<(i32, i32), PlaybackFact>,
) -> Result<(), StoreError> {
    let expected = derive(is_tv, available, facts);
    let current: Coordinates = tx
        .prepare_cached(
            "SELECT season,episode,position_ms,watch_activity_at
        FROM user_library_continue_units WHERE user_id=?1 AND library_id=?2 AND media_id=?3",
        )?
        .query_map(
            params![user.to_string(), library, media.to_string()],
            |row| Ok(((row.get(0)?, row.get(1)?), (row.get(2)?, row.get(3)?))),
        )?
        .collect::<Result<_, _>>()?;
    for &(season, episode) in current.keys().filter(|key| !expected.contains_key(*key)) {
        tx.execute(
            "DELETE FROM user_library_continue_units WHERE user_id=?1 AND library_id=?2
            AND media_id=?3 AND season=?4 AND episode=?5",
            params![
                user.to_string(),
                library,
                media.to_string(),
                season,
                episode
            ],
        )?;
    }
    for ((season, episode), state) in &expected {
        if current.get(&(*season, *episode)) == Some(state) {
            continue;
        }
        tx.execute("INSERT INTO user_library_continue_units
            (user_id,library_id,media_id,season,episode,position_ms,watch_activity_at)
            VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(user_id,library_id,media_id,season,episode)
            DO UPDATE SET position_ms=excluded.position_ms,watch_activity_at=excluded.watch_activity_at
            WHERE position_ms<>excluded.position_ms OR watch_activity_at<>excluded.watch_activity_at",
            params![user.to_string(),library,media.to_string(),season,episode,state.0,state.1])?;
    }
    Ok(())
}

use super::*;

pub(super) fn unit_played(
    store: &crate::Store,
    user_id: UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> bool {
    let mut units: [Option<crate::store::UnitState>; 3] = [None, None, None];
    let keys = [
        (season, episode),
        (season, crate::store::UNIT_WHOLE),
        (crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE),
    ];
    for (index, (unit_season, unit_episode)) in keys.into_iter().enumerate() {
        if index > 0 && (season < 0 || episode < 0) {
            continue;
        }
        match store.unit_state(user_id, media_id, unit_season, unit_episode) {
            Ok(Some(state)) => units[index] = Some(state),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, %user_id, %media_id, season = unit_season, episode = unit_episode, "failed to read Jellyfin played mark")
            }
        }
    }
    resolve_effective_played(
        units[0].as_ref().map(|unit| (unit, unit.updated_at)),
        units[1].as_ref().map(|unit| (unit, unit.updated_at)),
        units[2].as_ref().map(|unit| (unit, unit.updated_at)),
    )
}

fn carries_playback_authority(state: &crate::store::UnitState) -> bool {
    // A favourite-only insert has no playback facts. A genuine historical
    // played assertion can still have `updated_at = 0` and must count.
    state.updated_at != 0
        || state.played
        || state.position_ms != 0
        || state.play_count != 0
        || state.duration_ms.is_some()
        || state.audio_track.is_some()
        || state.subtitle_track.is_some()
}

pub(super) fn resolve_effective_played(
    episode_unit: Option<(&crate::store::UnitState, i64)>,
    season_unit: Option<(&crate::store::UnitState, i64)>,
    show_unit: Option<(&crate::store::UnitState, i64)>,
) -> bool {
    [episode_unit, season_unit, show_unit]
        .into_iter()
        .enumerate()
        .filter_map(|(specificity, unit)| {
            unit.filter(|(state, _)| carries_playback_authority(state))
                .map(|(state, updated_at)| (updated_at, 2 - specificity, state.played))
        })
        .max_by_key(|(updated_at, specificity, _)| (*updated_at, *specificity))
        .is_some_and(|(_, _, played)| played)
}
pub(super) fn item_is_played(
    store: &crate::Store,
    user_id: UserId,
    file: &domain::LibraryFile,
    unit: (i32, i32),
) -> bool {
    // 旧行模型的 `season/episode` 为 `None` 即整片；`UnitKey` 用 `-1` 表达同一件事。
    unit_played(store, user_id, file.media_id, unit.0, unit.1)
}

pub(super) fn unit_play_count(
    store: &crate::Store,
    user_id: UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
) -> i64 {
    match store.unit_state(user_id, media_id, season, episode) {
        Ok(Some(state)) => state.play_count,
        Ok(None) => 0,
        Err(error) => {
            tracing::warn!(%error, %user_id, %media_id, season, episode, "failed to read Jellyfin play count");
            0
        }
    }
}
pub(super) fn resolve_mark_unit(
    store: &crate::Store,
    user_id: UserId,
    id: &str,
) -> Result<Option<(domain::MediaId, i32, i32)>, String> {
    let Some(item) = domain::TypedItemId::from_wire_string(id) else {
        return Ok(None);
    };
    // Marks are writes: the named item must still be visible content for this
    // user, so a removed library, pending file or unknown id resolves to None.
    let visible = store
        .playable_sources(user_id, &item)
        .map_err(|error| error.to_string())?;
    if visible.is_empty() {
        return Ok(None);
    }
    Ok(Some(match item {
        domain::TypedItemId::Work(media) => {
            (media, crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE)
        }
        domain::TypedItemId::Season { media, season } => {
            (media, season as i32, crate::store::UNIT_WHOLE)
        }
        domain::TypedItemId::Unit { media, key } => (media, key.season, key.episode),
    }))
}

pub(super) fn write_favorite_mark(
    store: &crate::Store,
    user_id: UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
    favorite: bool,
    now: i64,
) -> Result<(), String> {
    store
        .set_unit_favorite(user_id, media_id, season, episode, favorite, now)
        .map_err(|error| {
            tracing::error!(%error, %user_id, %media_id, season, episode, "failed to update Jellyfin favorite mark");
            error.to_string()
        })
}

pub(super) fn persist_unplayed_override(
    store: &crate::Store,
    user_id: UserId,
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
    favorite: Option<bool>,
    now: i64,
) -> Result<(), String> {
    let current = store.unit_state(user_id, media_id, season, episode).map_err(|error| {
        tracing::error!(%error, %user_id, %media_id, season, episode, "failed to read Jellyfin mark after update");
        error.to_string()
    })?;
    if current.is_none() {
        store
            .upsert_unit(
                user_id, media_id, season, episode, 0, Some(false), favorite, None, None, None,
                false, now,
            )
            .map_err(|error| {
                tracing::error!(%error, %user_id, %media_id, season, episode, "failed to persist Jellyfin unplayed override");
                error.to_string()
            })?;
    }
    Ok(())
}
